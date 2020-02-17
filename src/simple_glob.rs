use std::iter::Iterator;

use anyhow::{Context, Result};
use git2::{Direction, Remote};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum ExpansionSide {
    Right,
    Left,
}

pub fn expand_refspec(
    remote: &Remote,
    reference: &str,
    direction: Direction,
    side: ExpansionSide,
) -> Result<Option<String>> {
    for refspec in remote.refspecs() {
        let left = refspec.src().context("non-utf8 src dst")?;
        let right = refspec.dst().context("non-utf8 refspec dst")?;
        // TODO: Why there isn't derive(Eq, PartialEq)?
        match (direction, refspec.direction()) {
            (Direction::Fetch, Direction::Push) | (Direction::Push, Direction::Fetch) => continue,
            _ => {}
        }
        match side {
            ExpansionSide::Right => return Ok(expand(&reference, left, right)),
            ExpansionSide::Left => return Ok(expand(&reference, right, left)),
        };
    }
    Ok(None)
}

fn expand(reference: &str, src: &str, dest: &str) -> Option<String> {
    assert_eq!(
        src.chars().filter(|&c| c == '*').count(),
        1,
        "Unsupported glob pattern: {}",
        src
    );
    assert_eq!(
        dest.chars().filter(|&c| c == '*').count(),
        1,
        "Unsupported glob pattern: {}",
        dest
    );
    let star = src.find('*').expect("assert number of '*' == 1");
    let left = &src[..star];
    let right = &src[star + 1..];
    let matched = if reference.starts_with(&left) && reference.ends_with(right) {
        &reference[left.len()..reference.len() - right.len()]
    } else {
        return None;
    };
    Some(dest.replace("*", matched))
}
