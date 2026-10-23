//! 图片的文件头判断。

use super::*;

#[test]
fn image_headers_are_recognized() {
    assert_eq!(media_type(b"\x89PNG"), Some("image/png"));
    assert_eq!(media_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
    assert_eq!(media_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
    assert_eq!(media_type(&[0xff, 0xd8, 0xff]), None, "不足 4 字节");
    assert_eq!(media_type(b"GIF87a.."), Some("image/gif"));
    assert_eq!(media_type(b"GIF89a.."), Some("image/gif"));
    assert_eq!(media_type(b"GIF8xa.."), None);
    assert_eq!(media_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(media_type(b"RIFF\0\0\0\0WAVE"), None);
    assert_eq!(media_type(b"plain"), None);
    assert_eq!(MAX_IMAGE_BYTES, 3_932_160);
}
