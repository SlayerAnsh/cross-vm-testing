pub fn format_port(chain_id: &str, address: &str) -> String {
    format!("{chain_id}.{address}")
}
