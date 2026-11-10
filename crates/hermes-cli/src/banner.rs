pub fn startup_banner() -> &'static str {
    "Reformed Agent"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_banner_names_reformed_agent() {
        assert_eq!(startup_banner(), "Reformed Agent");
    }
}
