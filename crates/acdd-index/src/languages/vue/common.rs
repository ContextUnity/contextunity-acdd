#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Position {
    pub(super) line: usize,
    pub(super) column: usize,
}

pub(super) fn source_position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    Position {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").len(),
    }
}
