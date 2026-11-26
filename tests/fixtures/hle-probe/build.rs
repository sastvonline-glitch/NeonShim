fn main() {
    println!("cargo:rustc-link-arg-cdylib=/DELAYLOAD:neonshim-probe-delayed.dll");
    println!("cargo:rustc-link-lib=delayimp");
}
