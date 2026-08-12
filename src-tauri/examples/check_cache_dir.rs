fn main() {
    let cache = dirs::cache_dir();
    println!("dirs::cache_dir() = {:?}", cache);
    let sdk = cache.unwrap().join("umeng-dau-client").join("sdk");
    println!("sdk_dir = {:?}", sdk);
    println!("cmdline-tools/latest/bin exists: {}", sdk.join("cmdline-tools/latest/bin").exists());
    println!("jre java exists: {}", sdk.join("jre/Contents/Home/bin/java").exists());
    println!("platform-tools/adb exists: {}", sdk.join("platform-tools/adb").exists());
    println!("emulator/emulator exists: {}", sdk.join("emulator/emulator").exists());
    println!("build-tools/34.0.0/aapt exists: {}", sdk.join("build-tools/34.0.0/aapt").exists());
}
