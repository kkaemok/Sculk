//! Fixed diagnostic labels and evidence bits; no user/peer strings are serializable.
use serde::Serialize;
use std::cell::Cell;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Configuration,
    ServerKey,
    SystemTrust,
    LocalValidationComplete,
    AccountAuthorization,
    RoomAuthorization,
    DeviceIdentity,
    PasswordInput,
    HttpsAuthentication,
    DeviceRegistrationApproval,
    DeviceChallenge,
    DeviceConfirmation,
    PostRegistrationLogin,
    ExplicitRefresh,
    Getconf,
    Checkin,
    Loginlist,
    RoomSelection,
    MessageReception,
    RoomPage,
    Shutdown,
    Complete,
}
impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::ServerKey => "server-key",
            Self::SystemTrust => "system-trust",
            Self::LocalValidationComplete => "local-validation-complete",
            Self::AccountAuthorization => "account-authorization",
            Self::RoomAuthorization => "room-authorization",
            Self::DeviceIdentity => "device-identity",
            Self::PasswordInput => "password-input",
            Self::HttpsAuthentication => "https-authentication",
            Self::DeviceRegistrationApproval => "device-registration-approval",
            Self::DeviceChallenge => "device-challenge",
            Self::DeviceConfirmation => "device-confirmation",
            Self::PostRegistrationLogin => "post-registration-login",
            Self::ExplicitRefresh => "explicit-refresh",
            Self::Getconf => "getconf",
            Self::Checkin => "checkin",
            Self::Loginlist => "loginlist",
            Self::RoomSelection => "room-selection",
            Self::MessageReception => "message-reception",
            Self::RoomPage => "room-page",
            Self::Shutdown => "shutdown",
            Self::Complete => "complete",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Milestones {
    pub configuration: bool,
    pub server_key: bool,
    pub https_authentication: bool,
    pub device_registration_approved: bool,
    pub getconf: bool,
    pub checkin: bool,
    pub loginlist: bool,
    pub selected_room: bool,
    pub controlled_message: bool,
    pub shutdown: bool,
}
pub struct Progress {
    pub stage: Cell<Stage>,
    milestones: Cell<Milestones>,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            stage: Cell::new(Stage::Configuration),
            milestones: Cell::new(Milestones::default()),
        }
    }
}
impl Progress {
    pub fn milestones(&self) -> Milestones {
        self.milestones.get()
    }
    pub fn completed(&self, stage: Stage) {
        let mut m = self.milestones.get();
        match stage {
            Stage::Configuration => m.configuration = true,
            Stage::ServerKey => m.server_key = true,
            Stage::HttpsAuthentication => m.https_authentication = true,
            Stage::DeviceRegistrationApproval => m.device_registration_approved = true,
            Stage::Getconf => m.getconf = true,
            Stage::Checkin => m.checkin = true,
            Stage::Loginlist => m.loginlist = true,
            Stage::RoomSelection => m.selected_room = true,
            Stage::MessageReception => m.controlled_message = true,
            Stage::Shutdown => m.shutdown = true,
            _ => {}
        }
        self.milestones.set(m);
    }
    // The existing connector calls its dialer for the next stage only AFTER the
    // prior command's reply and endpoint schema were validated.
    pub fn protocol_enter(&self, stage: sculk_client::connect::Stage) {
        let stage = match stage {
            sculk_client::connect::Stage::Booking => Stage::Getconf,
            sculk_client::connect::Stage::Checkin => {
                self.completed(Stage::Getconf);
                Stage::Checkin
            }
            sculk_client::connect::Stage::Login => {
                self.completed(Stage::Checkin);
                Stage::Loginlist
            }
        };
        self.stage.set(stage);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn milestones_never_infer_loco_login_or_reception_from_http_success() {
        let progress = Progress::default();
        progress.completed(Stage::HttpsAuthentication);
        let m = progress.milestones();
        assert!(m.https_authentication);
        assert!(!m.getconf && !m.loginlist && !m.controlled_message);
        progress.protocol_enter(sculk_client::connect::Stage::Booking);
        assert!(!progress.milestones().getconf);
        progress.protocol_enter(sculk_client::connect::Stage::Checkin);
        assert!(progress.milestones().getconf);
        assert!(!progress.milestones().checkin);
        progress.protocol_enter(sculk_client::connect::Stage::Login);
        assert!(progress.milestones().checkin);
        assert!(!progress.milestones().loginlist);
        progress.completed(Stage::Loginlist);
        assert!(progress.milestones().loginlist);
        assert!(!progress.milestones().controlled_message);
        let json = serde_json::to_string(&progress.milestones()).unwrap();
        for private in [
            "token",
            "password",
            "userId",
            "chatId",
            "approval_code",
            "message_content",
        ] {
            assert!(!json.contains(private));
        }
    }
}
