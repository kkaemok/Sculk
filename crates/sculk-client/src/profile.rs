//! Explicit connection contracts. See docs/advanced-compatibility.md for evidence.
use crate::{
    connect::{BookingSchema, DeviceProfile, Endpoint, Error, Options, Protection, Stage},
    session::{AccessToken, Session},
    tls::TlsSettings,
    Config,
};
use sculk_protocol::handshake::ServerKey;
use std::{fmt, future::Future, str::FromStr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{watch, Semaphore},
};

/// A named provider, transport, and wire schema combination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Profile {
    /// Historical Sculk Android HTTPS provider and LOCO GCM chat transport.
    #[default]
    AndroidLegacy,
    /// Android tablet HTTPS provider and verified TLS chat transport.
    AndroidTablet,
}

/// No matching profile or incompatible settings. Untrusted input is redacted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unsupported protocol profile or incompatible profile settings")]
pub struct UnsupportedProfile;

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AndroidLegacy => "android-legacy",
            Self::AndroidTablet => "android-tablet",
        })
    }
}

impl FromStr for Profile {
    type Err = UnsupportedProfile;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "android-legacy" => Ok(Self::AndroidLegacy),
            "android-tablet" => Ok(Self::AndroidTablet),
            _ => Err(UnsupportedProfile),
        }
    }
}

impl Profile {
    pub(crate) const TABLET_TLS_PORT: u16 = 10009;
    pub(crate) const TABLET_TLS_NAME: &str = "kakao.com";

    /// Fixed authorized route port for the TLS chat profile, when applicable.
    pub fn chat_tls_port(self) -> Option<u16> {
        (self == Self::AndroidTablet).then_some(Self::TABLET_TLS_PORT)
    }

    /// Both implemented profiles require the legitimate Android HTTPS provider.
    /// All fields shared by HTTPS and LOCO must describe that sub-device.
    pub fn validate_device(self, device: &DeviceProfile) -> Result<(), Error> {
        device.validate()?;
        if device.os != "android"
            || !device.use_sub
            || device.protocol_version != "1"
            || device.mccmnc != "99999"
            || device.language != "ko"
            || device.country_iso != "KR"
            || (self == Self::AndroidTablet && device.app_version != "25.9.2")
        {
            return Err(UnsupportedProfile.into());
        }
        Ok(())
    }
}

/// A profile bound to its device metadata and selected transports before
/// account authentication. All destinations still pass through the caller's
/// route policy. The key must already be independently trusted by the owner.
pub struct Connector {
    profile: Profile,
    device: DeviceProfile,
    options: Options,
    owner: Arc<Semaphore>,
    #[cfg(feature = "messaging")]
    sequence: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

/// Route and resource settings shared by both profiles. Wire dialect and
/// transport security are derived from the selected profile.
pub struct Settings {
    /// Exact TLS Booking destination.
    pub booking: Endpoint,
    /// Trust roots used for both Booking and tablet chat.
    pub tls: TlsSettings,
    /// Bounded request actor settings.
    pub transport: Config,
    /// Maximum dial/handshake/request time per stage.
    pub stage_timeout: Duration,
    /// Overall startup deadline.
    pub total_timeout: Duration,
    /// Local lease capped by credential expiry.
    pub session_lease: Duration,
}

impl Connector {
    /// Bind a profile using the trusted key for CHECKIN and, for legacy, chat.
    /// The profile owns the schema and transport selection.
    pub fn new(
        profile: Profile,
        device: DeviceProfile,
        key: ServerKey,
        settings: Settings,
    ) -> Result<Self, Error> {
        profile.validate_device(&device)?;
        let options = Options {
            booking: settings.booking,
            booking_schema: BookingSchema::AgentMessenger,
            checkin: Protection::LocoGcm(key.clone()),
            chat: match profile {
                Profile::AndroidLegacy => Protection::LocoGcm(key),
                Profile::AndroidTablet => Protection::Tls,
            },
            tls: settings.tls,
            transport: settings.transport,
            stage_timeout: settings.stage_timeout,
            total_timeout: settings.total_timeout,
            session_lease: settings.session_lease,
        };
        options.validate()?;
        Ok(Self {
            profile,
            device,
            options,
            owner: Arc::new(Semaphore::new(1)),
            #[cfg(feature = "messaging")]
            sequence: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(-1)),
        })
    }

    /// Profile selected before authentication.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// Use an already authorized credential for this profile and stable device.
    /// No credential acquisition, discovery fallback or retry is performed.
    pub async fn connect<S, F, Fut>(
        &self,
        token: AccessToken,
        device_uuid: &str,
        cancel: watch::Receiver<bool>,
        dial: F,
    ) -> Result<Session, Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnMut(Stage, Endpoint) -> Fut,
        Fut: Future<Output = Result<S, std::io::Error>>,
    {
        let owner = self
            .owner
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::SessionOwned)?;
        let mut session = self
            .options
            .connect_selected(
                &self.device,
                token,
                device_uuid,
                cancel,
                dial,
                Some(self.profile),
            )
            .await?;
        session.set_profile(self.profile, owner);
        #[cfg(feature = "messaging")]
        if self.profile == Profile::AndroidTablet {
            session.set_write_mode(crate::messaging::WriteMode::tablet(
                device_uuid,
                self.sequence.clone(),
            ));
        }
        Ok(session)
    }
}
