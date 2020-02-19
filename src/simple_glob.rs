use std::iter::Iterator;

use anyhow::{Context, Result};
use git2::{Direction, Remote};
use log::*;

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
            ExpansionSide::Right => return Ok(expand(left, right, &reference)),
            ExpansionSide::Left => return Ok(expand(right, left, &reference)),
        };
    }
    Ok(None)
}

fn expand(src: &str, dest: &str, reference: &str) -> Option<String> {
    let src_stars = src.chars().filter(|&c| c == '*').count();
    let dst_stars = dest.chars().filter(|&c| c == '*').count();
    assert!(
        src_stars <= 1 && src_stars == dst_stars,
        "Unsupported refspec patterns: {}:{}",
        src,
        dest
    );

    if let Some(matched) = simple_match(src, reference) {
        Some(dest.replace("*", matched))
    } else {
        None
    }
}

fn simple_match<'a>(pattern: &str, reference: &'a str) -> Option<&'a str> {
    let src_stars = pattern.chars().filter(|&c| c == '*').count();
    if src_stars <= 1 {
        if let Some(star) = pattern.find('*') {
            let left = &pattern[..star];
            let right = &pattern[star + 1..];
            if reference.starts_with(&left) && reference.ends_with(right) {
                let matched = &reference[left.len()..reference.len() - right.len()];
                return Some(matched);
            }
        } else if pattern == reference {
            return Some("");
        }
        return None;
    } else {
        warn!(
            "Unsupported refspec patterns, too many asterisks: {}",
            pattern
        );
    }
    None
}
