//! The Windows build of ONNX Runtime that gets linked in carries its DirectML provider, and
//! that calls into two system libraries which the `ort` crate names only when its own
//! `directml` feature is on. They are named here instead, so the feature (and the code it
//! brings) stays off.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=dylib=DirectML");
        println!("cargo:rustc-link-lib=dylib=dxcore");
    }
}
