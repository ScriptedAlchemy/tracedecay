use tracedecay_code_extraction::incremental::{ParseInputEdit, ParsePoint};

use crate::diagnostics::{PositionError, utf16_position_to_byte_offset};

use super::{OverlayChange, OverlayError};

pub(super) fn apply_change(
    text: &mut String,
    change: &OverlayChange,
) -> Result<ParseInputEdit, OverlayError> {
    let Some(range) = change.range else {
        if change.range_length.is_some() {
            return Err(OverlayError::RangeLengthWithoutRange);
        }
        let edit = ParseInputEdit {
            start_byte: 0,
            old_end_byte: text.len(),
            new_end_byte: change.text.len(),
            start_position: ParsePoint { row: 0, column: 0 },
            old_end_position: parse_point_at(text, text.len()),
            new_end_position: parse_point_at(&change.text, change.text.len()),
        };
        text.clone_from(&change.text);
        return Ok(edit);
    };
    let start =
        utf16_position_to_byte_offset(text, range.start).map_err(OverlayError::InvalidRange)?;
    let end = utf16_position_to_byte_offset(text, range.end).map_err(OverlayError::InvalidRange)?;
    if start > end {
        return Err(OverlayError::InvalidRange(
            PositionError::CharacterOutOfBounds,
        ));
    }
    if let Some(received) = change.range_length {
        let expected = text[start..end].encode_utf16().count() as u32;
        if expected != received {
            return Err(OverlayError::InvalidRangeLength { expected, received });
        }
    }
    let start_position = parse_point_at(text, start);
    let edit = ParseInputEdit {
        start_byte: start,
        old_end_byte: end,
        new_end_byte: start.saturating_add(change.text.len()),
        start_position,
        old_end_position: parse_point_at(text, end),
        new_end_position: replacement_end_point(start_position, &change.text),
    };
    text.replace_range(start..end, &change.text);
    Ok(edit)
}

fn parse_point_at(text: &str, byte_offset: usize) -> ParsePoint {
    let prefix = &text.as_bytes()[..byte_offset];
    let mut row = 0;
    let mut line_start = 0;
    for (index, byte) in prefix.iter().enumerate() {
        if *byte == b'\n' {
            row += 1;
            line_start = index + 1;
        }
    }
    ParsePoint {
        row,
        column: prefix.len().saturating_sub(line_start),
    }
}

fn replacement_end_point(start: ParsePoint, replacement: &str) -> ParsePoint {
    let end = parse_point_at(replacement, replacement.len());
    if end.row == 0 {
        ParsePoint {
            row: start.row,
            column: start.column.saturating_add(end.column),
        }
    } else {
        ParsePoint {
            row: start.row.saturating_add(end.row),
            column: end.column,
        }
    }
}
