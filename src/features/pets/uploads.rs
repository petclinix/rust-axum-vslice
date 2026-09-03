//! Decode/validate the inline base64 `picture`/`pictureContentType` fields,
//! store the decoded bytes outside `pets/<id>.json`, and re-encode them for
//! the read path (PLAN.md §9). The wire contract deliberately mirrors
//! `java-springboot-react-mtier`'s `PetRequest`/`Pet`; the on-disk layout
//! does not — the bytes live under `uploads/pets/<id>/`, never inlined in
//! the pet record itself.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use uuid::Uuid;

use crate::error::AppError;

/// 5 MiB — generous for a pet photo, small enough that a client can't use
/// this endpoint to fill the disk.
const MAX_PICTURE_BYTES: usize = 5 * 1024 * 1024;

const ALLOWED_CONTENT_TYPES: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

fn extension_for(content_type: &str) -> Option<&'static str> {
    ALLOWED_CONTENT_TYPES
        .iter()
        .find(|(ct, _)| *ct == content_type)
        .map(|(_, ext)| *ext)
}

fn picture_path(data_dir: &Path, pet_id: Uuid, content_type: &str) -> Option<PathBuf> {
    let ext = extension_for(content_type)?;
    Some(
        data_dir
            .join("uploads")
            .join("pets")
            .join(pet_id.to_string())
            .join(format!("picture.{ext}")),
    )
}

/// Validates `content_type` against the allowlist and `picture_base64`
/// against the size cap, returning the decoded bytes. Content type is
/// checked before decoding so a request with a bogus type is rejected
/// without doing the (larger) base64 work first.
pub fn decode_and_validate(picture_base64: &str, content_type: &str) -> Result<Vec<u8>, AppError> {
    if extension_for(content_type).is_none() {
        return Err(AppError::Validation(format!(
            "unsupported picture content type: {content_type}"
        )));
    }

    let bytes = BASE64
        .decode(picture_base64)
        .map_err(|_| AppError::Validation("picture is not valid base64".to_string()))?;

    if bytes.len() > MAX_PICTURE_BYTES {
        return Err(AppError::Validation(
            "picture exceeds the maximum allowed size".to_string(),
        ));
    }

    Ok(bytes)
}

/// Not JSON, so this doesn't go through `storage::atomic_write` — a torn
/// image write is a visible glitch, not a correctness invariant, so a plain
/// write is an acceptable trade against adding a second atomic-write
/// primitive just for bytes.
pub fn write_picture(
    data_dir: &Path,
    pet_id: Uuid,
    content_type: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let path = picture_path(data_dir, pet_id, content_type)
        .expect("content_type already validated by decode_and_validate");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

pub fn read_picture(
    data_dir: &Path,
    pet_id: Uuid,
    content_type: &str,
) -> io::Result<Option<Vec<u8>>> {
    let Some(path) = picture_path(data_dir, pet_id, content_type) else {
        return Ok(None);
    };
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn encode_base64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_validate_accepts_a_known_content_type() {
        let encoded = encode_base64(b"fake image bytes");

        let bytes = decode_and_validate(&encoded, "image/png").unwrap();

        assert_eq!(bytes, b"fake image bytes");
    }

    #[test]
    fn decode_and_validate_rejects_an_unknown_content_type() {
        let encoded = encode_base64(b"fake image bytes");

        let err = decode_and_validate(&encoded, "application/pdf").unwrap_err();

        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn decode_and_validate_rejects_invalid_base64() {
        let err = decode_and_validate("not-valid-base64!!!", "image/png").unwrap_err();

        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn decode_and_validate_rejects_oversized_pictures() {
        let too_big = vec![0u8; MAX_PICTURE_BYTES + 1];
        let encoded = encode_base64(&too_big);

        let err = decode_and_validate(&encoded, "image/png").unwrap_err();

        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn write_picture_then_read_picture_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let pet_id = Uuid::new_v4();

        write_picture(dir.path(), pet_id, "image/jpeg", b"bytes").unwrap();

        assert_eq!(
            read_picture(dir.path(), pet_id, "image/jpeg").unwrap(),
            Some(b"bytes".to_vec())
        );
    }

    #[test]
    fn read_picture_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            read_picture(dir.path(), Uuid::new_v4(), "image/jpeg").unwrap(),
            None
        );
    }

    #[test]
    fn different_pets_get_different_picture_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        write_picture(dir.path(), a, "image/png", b"a-bytes").unwrap();
        write_picture(dir.path(), b, "image/png", b"b-bytes").unwrap();

        assert_eq!(
            read_picture(dir.path(), a, "image/png").unwrap(),
            Some(b"a-bytes".to_vec())
        );
        assert_eq!(
            read_picture(dir.path(), b, "image/png").unwrap(),
            Some(b"b-bytes".to_vec())
        );
    }
}
