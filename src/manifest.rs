pub fn template(name: &str) -> String {
    format!(
        "[project]\nname = \"{}\"\nversion = \"0.1.0\"\n\n[dependencies]\n",
        name
    )
}
