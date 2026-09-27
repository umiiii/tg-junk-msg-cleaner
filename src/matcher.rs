//! Keyword matching.

use std::path::Path;

use anyhow::{bail, Context, Result};
use regex::{Regex, RegexSet, RegexSetBuilder};

#[derive(Debug, Clone, Copy, Default)]
pub struct MatchOptions {
    /// Treat keywords as regular expressions instead of literal text.
    pub regex: bool,
    pub case_sensitive: bool,
    /// A message must contain every keyword instead of any of them.
    pub require_all: bool,
}

pub struct Matcher {
    keywords: Vec<String>,
    set: RegexSet,
    options: MatchOptions,
}

impl Matcher {
    pub fn new(keywords: Vec<String>, options: MatchOptions) -> Result<Self> {
        let keywords = normalize_keywords(keywords);
        if keywords.is_empty() {
            bail!("at least one non-empty keyword is required");
        }
        let patterns: Vec<String> = if options.regex {
            for keyword in &keywords {
                Regex::new(keyword).with_context(|| format!("invalid regular expression {keyword:?}"))?;
            }
            keywords.clone()
        } else {
            keywords.iter().map(|keyword| regex::escape(keyword)).collect()
        };
        let set = RegexSetBuilder::new(&patterns)
            .case_insensitive(!options.case_sensitive)
            .build()
            .context("failed to compile the keywords")?;
        Ok(Self { keywords, set, options })
    }

    pub fn keywords(&self) -> &[String] {
        &self.keywords
    }

    pub fn options(&self) -> MatchOptions {
        self.options
    }

    /// Returns the keywords found in `text` if the text counts as a match.
    pub fn find(&self, text: &str) -> Option<Vec<&str>> {
        if text.is_empty() {
            return None;
        }
        let found: Vec<&str> = self
            .set
            .matches(text)
            .iter()
            .map(|i| self.keywords[i].as_str())
            .collect();
        let matched = if self.options.require_all {
            found.len() == self.keywords.len()
        } else {
            !found.is_empty()
        };
        matched.then_some(found)
    }
}

/// Trims keywords and drops empty ones and duplicates, keeping the original order.
pub fn normalize_keywords(keywords: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for keyword in keywords {
        let keyword = keyword.trim();
        if !keyword.is_empty() && !result.iter().any(|k| k == keyword) {
            result.push(keyword.to_owned());
        }
    }
    result
}

/// Reads keywords from a file: one per line, blank lines and lines starting with `#` are ignored.
pub fn read_keywords_file(path: &Path) -> Result<Vec<String>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("failed to read keywords file {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(keywords: &[&str], options: MatchOptions) -> Matcher {
        Matcher::new(keywords.iter().map(|k| k.to_string()).collect(), options).unwrap()
    }

    #[test]
    fn plain_keywords_are_literal_and_case_insensitive() {
        let m = matcher(&["Buy.Now", "加微信"], MatchOptions::default());
        assert_eq!(m.find("please buy.now!"), Some(vec!["Buy.Now"]));
        assert_eq!(m.find("buyXnow"), None);
        assert_eq!(m.find("日赚千元，加微信了解"), Some(vec!["加微信"]));
        assert_eq!(m.find("hello"), None);
        assert_eq!(m.find(""), None);
    }

    #[test]
    fn case_sensitive() {
        let m = matcher(
            &["USDT"],
            MatchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert!(m.find("free USDT").is_some());
        assert!(m.find("free usdt").is_none());
    }

    #[test]
    fn regex_mode() {
        let m = matcher(
            &[r"t\.me/\w+bot", r"\d{5,}"],
            MatchOptions {
                regex: true,
                ..Default::default()
            },
        );
        assert_eq!(m.find("join T.ME/SpamBot"), Some(vec![r"t\.me/\w+bot"]));
        assert_eq!(m.find("call 1234567"), Some(vec![r"\d{5,}"]));
        assert!(m.find("nothing here 123").is_none());
        let err = Matcher::new(
            vec!["(".into()],
            MatchOptions {
                regex: true,
                ..Default::default()
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn require_all() {
        let m = matcher(
            &["crypto", "profit"],
            MatchOptions {
                require_all: true,
                ..Default::default()
            },
        );
        assert!(m.find("crypto news").is_none());
        assert_eq!(m.find("Crypto PROFIT daily"), Some(vec!["crypto", "profit"]));
    }

    #[test]
    fn keywords_are_normalized() {
        assert_eq!(
            normalize_keywords(vec![" a ".into(), "".into(), "a".into(), "b".into(), "  ".into()]),
            vec!["a", "b"]
        );
        assert!(Matcher::new(vec![" ".into()], MatchOptions::default()).is_err());
    }
}
