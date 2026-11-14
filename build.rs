fn main() {
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_REVISION");
    println!("cargo:rerun-if-changed=frontend/dist");
    if !std::path::Path::new("frontend/dist/index.html").exists() {
        panic!("Frontend assets missing. Run: cd frontend && npm ci && npm run build");
    }
}
