//! Private asset handles and bounded media planning. Transfer requires a
//! separately verified media endpoint and receipt contract.
use rustix::fs::{open, openat, Mode, OFlags};
use std::{
    fs::File,
    io::{Read, Seek, Take},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    Single(Kind),
    ImageGroup,
    Sequential,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("asset path is outside policy or could not be opened")]
    Path,
    #[error("asset length or attachment count exceeds its bounds")]
    Limit,
    #[error("unknown or mismatched asset content type")]
    ContentType,
    #[error("asset could not be read")]
    Read,
}

/// An operator-selected directory. Files are opened relative to held directory
/// descriptors, with symlink refusal on every path component.
pub struct Assets(File);

impl Assets {
    pub fn open(root: &Path) -> Result<Self, Error> {
        let mut parts = root.components();
        if parts.next() != Some(Component::RootDir) {
            return Err(Error::Path);
        }
        let mut directory =
            File::from(open("/", directory_flags(), Mode::empty()).map_err(|_| Error::Path)?);
        for part in parts {
            let Component::Normal(name) = part else {
                return Err(Error::Path);
            };
            directory = File::from(
                openat(&directory, name, directory_flags(), Mode::empty())
                    .map_err(|_| Error::Path)?,
            );
        }
        if root == Path::new("/") {
            return Err(Error::Path);
        }
        Ok(Self(directory))
    }

    pub fn file(&self, path: &Path, mime: &str, max_bytes: u64) -> Result<Asset, Error> {
        if max_bytes == 0 || max_bytes > MAX_FILE || path.as_os_str().len() > 1024 {
            return Err(Error::Limit);
        }
        let mut parts = path.components().peekable();
        let mut directory = self.0.try_clone().map_err(|_| Error::Path)?;
        let name = loop {
            let Some(Component::Normal(name)) = parts.next() else {
                return Err(Error::Path);
            };
            if parts.peek().is_none() {
                break name;
            }
            directory = File::from(
                openat(&directory, name, directory_flags(), Mode::empty())
                    .map_err(|_| Error::Path)?,
            );
        };
        let display = name
            .to_str()
            .filter(|name| name.len() <= 255 && !name.chars().any(char::is_control))
            .ok_or(Error::Path)?;
        let mut file = File::from(
            openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|_| Error::Path)?,
        );
        let metadata = file.metadata().map_err(|_| Error::Path)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(Error::Path);
        }
        if metadata.len() == 0 || metadata.len() > max_bytes {
            return Err(Error::Limit);
        }
        let mut header = [0_u8; 32];
        let header = &mut header[..metadata.len().min(32) as usize];
        file.read_exact(header).map_err(|_| Error::Read)?;
        file.rewind().map_err(|_| Error::Read)?;
        let mime = mime.to_ascii_lowercase();
        let kind = content_kind(&mime, header).ok_or(Error::ContentType)?;
        Ok(Asset {
            file,
            name: display.to_owned(),
            mime,
            kind,
            bytes: metadata.len(),
        })
    }
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// One validated descriptor. The original path is never reopened.
pub struct Asset {
    file: File,
    name: String,
    mime: String,
    kind: Kind,
    bytes: u64,
}

impl Asset {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn mime(&self) -> &str {
        &self.mime
    }
    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Stream at most the validated size in the caller's bounded chunks.
    /// A future transport must detect short reads and verify its own receipt.
    pub fn into_reader(self) -> Take<File> {
        self.file.take(self.bytes)
    }
}

fn content_kind(mime: &str, header: &[u8]) -> Option<Kind> {
    match mime {
        "image/png" if header.starts_with(b"\x89PNG\r\n\x1a\n") => Some(Kind::Image),
        "image/jpeg" if header.starts_with(b"\xff\xd8\xff") => Some(Kind::Image),
        "image/gif" if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") => {
            Some(Kind::Image)
        }
        "image/webp" if riff(header, b"WEBP") => Some(Kind::Image),
        "audio/wav" if riff(header, b"WAVE") => Some(Kind::Audio),
        "audio/mpeg" if header.starts_with(b"ID3") => Some(Kind::Audio),
        "audio/mp4" if header.get(4..12) == Some(b"ftypM4A ") => Some(Kind::Audio),
        "video/mp4"
            if header
                .get(4..12)
                .is_some_and(|part| matches!(part, b"ftypmp42" | b"ftypisom")) =>
        {
            Some(Kind::Video)
        }
        "application/pdf" if header.starts_with(b"%PDF-") => Some(Kind::File),
        "application/octet-stream" => Some(Kind::File),
        _ => None,
    }
}

fn riff(header: &[u8], format: &[u8]) -> bool {
    header.starts_with(b"RIFF") && header.get(8..12) == Some(format)
}

/// Validated attachment ordering and resource budget, before transmission.
pub struct Plan {
    route: Route,
    files: Vec<Asset>,
}

impl Plan {
    pub fn new(files: Vec<Asset>) -> Result<Self, Error> {
        if !(1..=16).contains(&files.len())
            || files.iter().map(Asset::bytes).sum::<u64>() > MAX_TOTAL
        {
            return Err(Error::Limit);
        }
        let route = if files.len() == 1 {
            Route::Single(files[0].kind())
        } else if files.iter().all(|asset| asset.kind() == Kind::Image) {
            Route::ImageGroup
        } else {
            Route::Sequential
        };
        Ok(Self { route, files })
    }

    pub fn route(&self) -> Route {
        self.route
    }
    pub fn files(&self) -> &[Asset] {
        &self.files
    }
    pub fn into_files(self) -> Vec<Asset> {
        self.files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_verified_asset_containers_and_refuses_path_escape() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("sculk-assets-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let picture = root.join("picture.png");
        std::fs::write(&picture, b"\x89PNG\r\n\x1a\nsynthetic").unwrap();
        std::fs::write(root.join("file.txt"), b"private fixture").unwrap();
        symlink(&picture, root.join("alias")).unwrap();
        let assets = Assets::open(&root).unwrap();
        let pic = || {
            assets
                .file(Path::new("picture.png"), "image/png", 1024)
                .unwrap()
        };
        assert_eq!(
            Plan::new(vec![pic(), pic()]).unwrap().route(),
            Route::ImageGroup
        );
        let generic = assets
            .file(Path::new("file.txt"), "application/octet-stream", 1024)
            .unwrap();
        assert_eq!(
            Plan::new(vec![pic(), generic]).unwrap().route(),
            Route::Sequential
        );
        assert!(matches!(
            assets.file(Path::new("alias"), "image/png", 1024),
            Err(Error::Path)
        ));
        assert!(matches!(
            assets.file(Path::new("../picture.png"), "image/png", 1024),
            Err(Error::Path)
        ));
        assert!(matches!(
            assets.file(Path::new("file.txt"), "image/png", 1024),
            Err(Error::ContentType)
        ));
        assert!(matches!(
            assets.file(Path::new("picture.png"), "image/png", 4),
            Err(Error::Limit)
        ));
        let mut data = Vec::new();
        pic().into_reader().read_to_end(&mut data).unwrap();
        assert!(data.starts_with(b"\x89PNG"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
