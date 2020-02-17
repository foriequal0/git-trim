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
    let src_stars = src.chars().filter(|&c| c == '*').count();
    let dst_stars = dest.chars().filter(|&c| c == '*').count();
    assert!(
        src_stars <= 1 && src_stars == dst_stars,
        "Unsupported refspec patterns: {}:{}",
        src,
        dest
    );
    if let Some(star) = src.find('*') {
        let left = &src[..star];
        let right = &src[star + 1..];
        if reference.starts_with(&left) && reference.ends_with(right) {
            let matched = &reference[left.len()..reference.len() - right.len()];
            return Some(dest.replace("*", matched));
        }
    } else if src == reference {
        return Some(dest.to_string());
    }
    None
}
