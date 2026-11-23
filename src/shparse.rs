//! Splits a shell line into pipeline stages, an output redirection and the background mark. Pure, host-tested.
//!
//! Grammar: `stage ( '|' stage )* ( ('>' | '>>') file )? '&'?`. Inside double quotes the operator
//! characters are ordinary text, and the quotes stay in the stage so the command sees them as typed.
//! `&&`, `||` and `<` are not supported and are refused by name rather than guessed at.

use alloc::vec::Vec;

#[derive(Debug, PartialEq, Eq)]
pub struct Line<'a> {
    /// One or more commands; the output of each is the input of the next.
    pub stages: Vec<&'a str>,
    /// Where the last stage's output goes: the file and whether to append.
    pub redirect: Option<(&'a str, bool)>,
    pub background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// `|` with nothing before or after it.
    EmptyStage,
    /// `>` with no file name, or with more than one word after it.
    BadRedirect,
    /// A second `>`: only one redirection is allowed, and it comes last.
    TwoRedirects,
    /// `&&`, `||` or `<`.
    Unsupported(&'static str),
    /// `&` anywhere but at the very end.
    StrayAmpersand,
    UnclosedQuote,
}

impl ParseError {
    pub fn message(self) -> &'static str {
        match self {
            ParseError::EmptyStage => "nothing to run on one side of `|`",
            ParseError::BadRedirect => "`>` needs exactly one file name after it",
            ParseError::TwoRedirects => "only one `>` is allowed, at the end",
            ParseError::Unsupported("&&") => "`&&` is not supported; run the commands one by one",
            ParseError::Unsupported("||") => "`||` is not supported; run the commands one by one",
            ParseError::Unsupported(_) => "`<` is not supported; use a pipe, e.g. `cat file | prog`",
            ParseError::StrayAmpersand => "`&` (run in the background) must be the last thing on the line",
            ParseError::UnclosedQuote => "a double quote is not closed",
        }
    }
}

/// True when the line has nothing for this parser to do: no operator outside quotes.
pub fn is_plain(line: &str) -> bool {
    let mut quoted = false;
    for b in line.bytes() {
        match b {
            b'"' => quoted = !quoted,
            b'|' | b'>' | b'<' | b'&' if !quoted => return false,
            _ => {}
        }
    }
    true
}

pub fn parse(line: &str) -> Result<Line<'_>, ParseError> {
    let b = line.as_bytes();
    let mut stages = Vec::new();
    let mut redirect = None;
    let mut background = false;
    let mut quoted = false;
    let mut start = 0usize;
    // Set once `>` was seen: the rest (up to an optional final `&`) is the file name.
    let mut redir_at: Option<(usize, bool)> = None;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            quoted = !quoted;
        } else if !quoted {
            match c {
                b'|' => {
                    if b.get(i + 1) == Some(&b'|') {
                        return Err(ParseError::Unsupported("||"));
                    }
                    if redir_at.is_some() {
                        return Err(ParseError::TwoRedirects);
                    }
                    let s = line[start..i].trim();
                    if s.is_empty() {
                        return Err(ParseError::EmptyStage);
                    }
                    stages.push(s);
                    start = i + 1;
                }
                b'>' => {
                    if redir_at.is_some() {
                        return Err(ParseError::TwoRedirects);
                    }
                    let s = line[start..i].trim();
                    if s.is_empty() {
                        return Err(ParseError::EmptyStage);
                    }
                    stages.push(s);
                    let append = b.get(i + 1) == Some(&b'>');
                    if append {
                        i += 1;
                    }
                    redir_at = Some((i + 1, append));
                }
                b'<' => return Err(ParseError::Unsupported("<")),
                b'&' => {
                    if b.get(i + 1) == Some(&b'&') {
                        return Err(ParseError::Unsupported("&&"));
                    }
                    if !line[i + 1..].trim().is_empty() {
                        return Err(ParseError::StrayAmpersand);
                    }
                    background = true;
                    let tail_end = i;
                    finish(line, start, tail_end, redir_at, &mut stages, &mut redirect)?;
                    return Ok(Line { stages, redirect, background });
                }
                _ => {}
            }
        }
        i += 1;
    }
    if quoted {
        return Err(ParseError::UnclosedQuote);
    }
    finish(line, start, b.len(), redir_at, &mut stages, &mut redirect)?;
    Ok(Line { stages, redirect, background })
}

fn finish<'a>(
    line: &'a str,
    start: usize,
    end: usize,
    redir_at: Option<(usize, bool)>,
    stages: &mut Vec<&'a str>,
    redirect: &mut Option<(&'a str, bool)>,
) -> Result<(), ParseError> {
    match redir_at {
        Some((at, append)) => {
            let name = line[at..end].trim();
            if name.is_empty() || name.split_whitespace().count() != 1 {
                return Err(ParseError::BadRedirect);
            }
            *redirect = Some((name, append));
        }
        None => {
            let s = line[start..end].trim();
            if s.is_empty() {
                return Err(ParseError::EmptyStage);
            }
            stages.push(s);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(line: &str) -> Line<'_> {
        parse(line).unwrap()
    }

    #[test]
    fn a_plain_command_is_one_stage() {
        assert_eq!(ok("ls /bin"), Line { stages: vec!["ls /bin"], redirect: None, background: false });
        assert!(is_plain("ls /bin"));
        assert!(is_plain("echo \"a | b > c & d\""));
        assert!(!is_plain("ls | wc"));
    }

    #[test]
    fn pipes_split_stages() {
        assert_eq!(ok("ps | grep shell | wc").stages, vec!["ps", "grep shell", "wc"]);
        assert_eq!(ok("a|b").stages, vec!["a", "b"]);
    }

    #[test]
    fn redirection_truncate_and_append() {
        assert_eq!(ok("ls > /tmp/out").redirect, Some(("/tmp/out", false)));
        assert_eq!(ok("ls >> /tmp/out").redirect, Some(("/tmp/out", true)));
        let l = ok("ps | grep idle >log.txt");
        assert_eq!(l.stages, vec!["ps", "grep idle"]);
        assert_eq!(l.redirect, Some(("log.txt", false)));
    }

    #[test]
    fn background_mark() {
        let l = ok("spin &");
        assert_eq!((l.stages.clone(), l.background), (vec!["spin"], true));
        let l = ok("hello > out.txt &");
        assert_eq!((l.redirect, l.background), (Some(("out.txt", false)), true));
    }

    #[test]
    fn quotes_protect_operators() {
        let l = ok("write a.txt \"x > y | z & w\"");
        assert_eq!(l.stages, vec!["write a.txt \"x > y | z & w\""]);
        assert_eq!(l.redirect, None);
        assert!(!l.background);
        assert_eq!(ok("echo \"a|b\" | wc").stages, vec!["echo \"a|b\"", "wc"]);
    }

    #[test]
    fn errors_are_specific() {
        assert_eq!(parse("| wc"), Err(ParseError::EmptyStage));
        assert_eq!(parse("ls |"), Err(ParseError::EmptyStage));
        assert_eq!(parse("ls | | wc"), Err(ParseError::EmptyStage));
        assert_eq!(parse("ls >"), Err(ParseError::BadRedirect));
        assert_eq!(parse("ls > a b"), Err(ParseError::BadRedirect));
        assert_eq!(parse("> a"), Err(ParseError::EmptyStage));
        assert_eq!(parse("ls > a > b"), Err(ParseError::TwoRedirects));
        assert_eq!(parse("ls > a | wc"), Err(ParseError::TwoRedirects));
        assert_eq!(parse("a && b"), Err(ParseError::Unsupported("&&")));
        assert_eq!(parse("a || b"), Err(ParseError::Unsupported("||")));
        assert_eq!(parse("wc < a"), Err(ParseError::Unsupported("<")));
        assert_eq!(parse("a & b"), Err(ParseError::StrayAmpersand));
        assert_eq!(parse("&"), Err(ParseError::EmptyStage));
        assert_eq!(parse("echo \"abc"), Err(ParseError::UnclosedQuote));
        for e in [ParseError::EmptyStage, ParseError::BadRedirect, ParseError::TwoRedirects, ParseError::Unsupported("<"), ParseError::StrayAmpersand, ParseError::UnclosedQuote] {
            assert!(!e.message().is_empty());
        }
    }

    #[test]
    fn never_panics_on_odd_input() {
        for s in ["", " ", ">>", ">>>", "|&", "&&&", "\"", "\"\"", "a>>", "a>>b&", "é | ü > ñ"] {
            let _ = parse(s);
            let _ = is_plain(s);
        }
        assert_eq!(ok("é | ü > ñ").stages, vec!["é", "ü"]);
    }
}
