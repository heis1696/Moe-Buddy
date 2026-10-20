fn main() {
    use std::{env, fs, path::PathBuf};
    let mut icon = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    icon.push("prototype.ico");
    let pixel_bytes = 16 * 16 * 4;
    let mask_bytes = 16 * 4;
    let image_bytes = 40 + pixel_bytes + mask_bytes;
    let mut bytes = vec![0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0];
    bytes.extend_from_slice(&(image_bytes as u32).to_le_bytes());
    bytes.extend_from_slice(&22u32.to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&16i32.to_le_bytes());
    bytes.extend_from_slice(&32i32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    for _ in 0..16 {
        for _ in 0..16 {
            bytes.extend_from_slice(&[128, 128, 128, 255]);
        }
    }
    bytes.extend_from_slice(&vec![0; mask_bytes]);
    fs::write(&icon, bytes).expect("write temporary Windows resource icon");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon)),
    )
    .expect("Tauri build configuration");
}
