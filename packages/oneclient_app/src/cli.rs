#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cli {
    pub launch: Option<String>,
    /// Start only the paired local agent, then exit without opening the window.
    pub agent_only: bool,
}

impl Cli {
    fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut cli = Self::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            if arg == "--agent-only" {
                cli.agent_only = true;
            } else if let Some(value) = arg.strip_prefix("--launch=") {
                cli.launch = non_empty(value.to_string());
            } else if arg == "--launch" {
                cli.launch = args.next().and_then(non_empty);
            } else if let Some(folder) = crate::protocol::parse_launch_url(&arg) {
                cli.launch = Some(folder);
            }
        }

        cli
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[must_use]
pub fn parse() -> Cli {
    Cli::from_args(std::env::args().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::from_args(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn no_arguments_is_an_ordinary_start() {
        assert_eq!(parse(&[]).launch, None);
    }

    #[test]
    fn both_spellings_carry_the_folder() {
        assert_eq!(
            parse(&["--launch", "fabric-1-20"]).launch.as_deref(),
            Some("fabric-1-20")
        );
        assert_eq!(
            parse(&["--launch=fabric-1-20"]).launch.as_deref(),
            Some("fabric-1-20")
        );
    }

    #[test]
    fn a_blank_folder_is_no_request_at_all() {
        assert_eq!(parse(&["--launch", "   "]).launch, None);
        assert_eq!(parse(&["--launch="]).launch, None);
        assert_eq!(parse(&["--launch"]).launch, None);
    }

    #[test]
    fn spaces_survive_the_round_trip() {
        assert_eq!(
            parse(&["--launch", "My Pack (1.8.9)"]).launch.as_deref(),
            Some("My Pack (1.8.9)"),
        );
    }

    #[test]
    fn unknown_flags_are_ignored_not_fatal() {
        assert_eq!(
            parse(&["--verbose", "--launch", "x", "leftover"])
                .launch
                .as_deref(),
            Some("x")
        );
    }

    #[test]
    fn agent_only_is_explicit_and_does_not_imply_a_launch() {
        let cli = parse(&["--agent-only"]);
        assert!(cli.agent_only);
        assert_eq!(cli.launch, None);
        assert!(!parse(&[]).agent_only);
    }

    #[test]
    fn a_launch_url_is_accepted_as_a_bare_argument() {
        let url = crate::protocol::launch_url("26.1.2 Fabric");
        assert_eq!(parse(&[&url]).launch.as_deref(), Some("26.1.2 Fabric"));
    }

    #[test]
    fn a_url_that_is_not_ours_is_not_a_launch() {
        assert_eq!(parse(&["https://polyfrost.org"]).launch, None);
    }
}
