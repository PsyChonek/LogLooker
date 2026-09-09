//! Extracted field values, stored one column per field.
//!
//! A result holds millions of hits, so how a hit's fields are stored is a memory
//! budget rather than a detail. Values live in per-field columns instead of in
//! the hits: text is interned to a `u32` dictionary index, numbers to an `f64`,
//! which is 4-8 bytes per field per hit with no per-hit allocation at all.
//!
//! Hits reference their values by row index (`CompactHit::row`) rather than by
//! position, so re-sorting a result reorders the hits and leaves the columns
//! alone.

use crate::plugin::{CompiledField, FieldType};
use serde::Serialize;
use std::collections::HashMap;

/// Absent text value. A real dictionary never reaches this index: it would need
/// four billion distinct values in one column.
const NO_TEXT: u32 = u32::MAX;

/// One column of a result, as the UI needs to describe it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldMeta {
    /// `<packId>.<key>`
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
}

impl FieldMeta {
    pub fn of(field: &CompiledField) -> Self {
        FieldMeta {
            key: field.key.clone(),
            label: field.label.clone(),
            kind: field.kind,
        }
    }

    pub fn is_number(&self) -> bool {
        self.kind == FieldType::Number
    }
}

/// One extracted value, as a row holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Extracted<'a> {
    Text(&'a str),
    Number(f64),
}

/// What a row's slot currently holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Empty,
    Text,
    Number,
}

/// One column's in-flight value. The `String` is kept across rows and reused, so
/// a scan that extracts on every hit stops allocating once it is warm.
#[derive(Debug, Clone)]
struct Slot {
    kind: SlotKind,
    text: String,
    number: f64,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            kind: SlotKind::Empty,
            text: String::new(),
            number: 0.0,
        }
    }
}

/// The in-flight values of one hit, indexed by column. Owned rather than
/// borrowed from the line, because the row outlives the line it was read from -
/// the scanner reuses one row for every hit in a file.
#[derive(Debug, Clone, Default)]
pub struct FieldRow {
    values: Vec<Slot>,
}

impl FieldRow {
    pub fn new(columns: usize) -> Self {
        FieldRow {
            values: vec![Slot::default(); columns],
        }
    }

    /// Marks every slot absent, keeping the text buffers for the next hit.
    pub fn clear(&mut self) {
        self.values
            .iter_mut()
            .for_each(|slot| slot.kind = SlotKind::Empty);
    }

    pub fn set(&mut self, column: usize, value: Extracted) {
        let Some(slot) = self.values.get_mut(column) else {
            return;
        };
        match value {
            Extracted::Text(text) => {
                slot.kind = SlotKind::Text;
                slot.text.clear();
                slot.text.push_str(text);
            }
            Extracted::Number(number) => {
                slot.kind = SlotKind::Number;
                slot.number = number;
            }
        }
    }

    /// Writes text lowercased straight into the slot's buffer, so a GUID written
    /// in two cases is one value without an allocation to fold it.
    pub fn set_lowercase(&mut self, column: usize, value: &str) {
        let Some(slot) = self.values.get_mut(column) else {
            return;
        };
        slot.kind = SlotKind::Text;
        slot.text.clear();
        slot.text
            .extend(value.chars().map(|c| c.to_ascii_lowercase()));
    }

    pub fn is_empty(&self) -> bool {
        self.values
            .iter()
            .all(|slot| slot.kind == SlotKind::Empty)
    }

    fn get(&self, column: usize) -> Option<Extracted<'_>> {
        let slot = self.values.get(column)?;
        match slot.kind {
            SlotKind::Empty => None,
            SlotKind::Text => Some(Extracted::Text(&slot.text)),
            SlotKind::Number => Some(Extracted::Number(slot.number)),
        }
    }
}

#[derive(Debug)]
enum Column {
    Text(TextColumn),
    /// Absent is a non-finite value; a parsed number is only stored when finite,
    /// so "NaN" in a log cannot masquerade as a real measurement either way
    Number(Vec<f64>),
}

#[derive(Debug, Default)]
struct TextColumn {
    /// One entry per row; `NO_TEXT` where the row has no value
    values: Vec<u32>,
    dict: Vec<Box<str>>,
    index: HashMap<Box<str>, u32>,
    /// The value interned last. Log lines repeat their categories in runs, so
    /// this skips the hash for most rows.
    recent: Option<u32>,
}

impl TextColumn {
    fn intern(&mut self, value: &str) -> u32 {
        if let Some(recent) = self.recent {
            if &*self.dict[recent as usize] == value {
                return recent;
            }
        }
        let id = match self.index.get(value) {
            Some(id) => *id,
            None => {
                let id = self.dict.len() as u32;
                let owned: Box<str> = value.into();
                self.dict.push(owned.clone());
                self.index.insert(owned, id);
                id
            }
        };
        self.recent = Some(id);
        id
    }
}

/// Every field's values for one result.
#[derive(Debug)]
pub struct FieldColumns {
    columns: Vec<Column>,
    rows: usize,
}

impl FieldColumns {
    pub fn new(metas: &[FieldMeta]) -> Self {
        FieldColumns {
            columns: metas
                .iter()
                .map(|meta| match meta.is_number() {
                    true => Column::Number(Vec::new()),
                    false => Column::Text(TextColumn::default()),
                })
                .collect(),
            rows: 0,
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// How many columns a row has.
    pub fn width(&self) -> usize {
        self.columns.len()
    }

    /// Appends `row` as a new row and returns its index.
    pub fn push_row(&mut self, row: &FieldRow) -> u32 {
        let at = self.rows;
        for (column, slot) in self.columns.iter_mut().enumerate() {
            match slot {
                Column::Text(text) => {
                    let id = match row.get(column) {
                        Some(Extracted::Text(value)) => text.intern(value),
                        // A number landing in a text column is still a value the
                        // user searched for, so keep it as its own text
                        Some(Extracted::Number(value)) => text.intern(&value.to_string()),
                        None => NO_TEXT,
                    };
                    text.values.push(id);
                }
                Column::Number(numbers) => {
                    let value = match row.get(column) {
                        Some(Extracted::Number(value)) if value.is_finite() => value,
                        // Text in a number column is not a measurement
                        _ => f64::NAN,
                    };
                    numbers.push(value);
                }
            }
        }
        self.rows += 1;
        at as u32
    }

    /// Folds a later match of the same entry into the last row: values already
    /// there win, because the line that opened the entry is the one that
    /// describes it.
    pub fn merge_into_last(&mut self, row: &FieldRow) {
        if self.rows == 0 {
            self.push_row(row);
            return;
        }
        let last = self.rows - 1;
        for (column, slot) in self.columns.iter_mut().enumerate() {
            let Some(value) = row.get(column) else { continue };
            match slot {
                Column::Text(text) => {
                    if text.values[last] != NO_TEXT {
                        continue;
                    }
                    text.values[last] = match value {
                        Extracted::Text(value) => text.intern(value),
                        Extracted::Number(value) => text.intern(&value.to_string()),
                    };
                }
                Column::Number(numbers) => {
                    if numbers[last].is_finite() {
                        continue;
                    }
                    if let Extracted::Number(value) = value {
                        if value.is_finite() {
                            numbers[last] = value;
                        }
                    }
                }
            }
        }
    }

    /// Folds `source` into `target`, values already in the target winning. Used
    /// when stitching parallel scan units finds two hits that turn out to be the
    /// same entry: the rows exist already, so they are merged in place and the
    /// source row is simply left unreferenced.
    pub fn merge_rows(&mut self, target: u32, source: u32) {
        if target == source {
            return;
        }
        let (target, source) = (target as usize, source as usize);
        for slot in &mut self.columns {
            match slot {
                Column::Text(text) => {
                    let (Some(&into), Some(&from)) =
                        (text.values.get(target), text.values.get(source))
                    else {
                        continue;
                    };
                    if into == NO_TEXT && from != NO_TEXT {
                        text.values[target] = from;
                    }
                }
                Column::Number(numbers) => {
                    let (Some(&into), Some(&from)) =
                        (numbers.get(target), numbers.get(source))
                    else {
                        continue;
                    };
                    if !into.is_finite() && from.is_finite() {
                        numbers[target] = from;
                    }
                }
            }
        }
    }

    /// Concatenates another set of columns, as stitching parallel scan units
    /// does, and returns the row offset its rows moved by. Dictionaries were
    /// built independently, so text indices are remapped rather than copied.
    pub fn append(&mut self, other: FieldColumns) -> u32 {
        let offset = self.rows as u32;
        for (slot, incoming) in self.columns.iter_mut().zip(other.columns) {
            match (slot, incoming) {
                (Column::Text(text), Column::Text(incoming)) => {
                    let remap: Vec<u32> = incoming
                        .dict
                        .iter()
                        .map(|value| text.intern(value))
                        .collect();
                    text.values.extend(incoming.values.into_iter().map(|id| {
                        match id {
                            NO_TEXT => NO_TEXT,
                            id => remap[id as usize],
                        }
                    }));
                }
                (Column::Number(numbers), Column::Number(incoming)) => {
                    numbers.extend(incoming);
                }
                // Both sides are built from the same field list, so the kinds
                // always line up; a mismatch would be a bug, not bad input
                (slot, _) => debug_assert!(false, "column kind mismatch in {slot:?}"),
            }
        }
        self.rows += other.rows;
        offset
    }

    /// Which columns hold a value on at least one of `rows`, in column order.
    /// A field none of a result's hits carried would be an empty table column,
    /// so the UI leaves it out; whitespace counts as no value, because that is
    /// what a blank cell would show.
    ///
    /// The rows are the ones the hits point at rather than every stored row:
    /// narrowing drops hits without touching the columns, so the rows they left
    /// behind must not keep a column alive.
    pub fn populated(&self, rows: impl IntoIterator<Item = u32>) -> Vec<bool> {
        let mut found = vec![false; self.columns.len()];
        let mut left = self.columns.len();
        if left == 0 {
            return found;
        }
        for row in rows {
            for (column, filled) in found.iter_mut().enumerate() {
                if *filled || !self.has_value(column, row) {
                    continue;
                }
                *filled = true;
                left -= 1;
            }
            // Every column has a value somewhere; nothing left to learn
            if left == 0 {
                break;
            }
        }
        found
    }

    fn has_value(&self, column: usize, row: u32) -> bool {
        match self.columns.get(column) {
            Some(Column::Text(_)) => self.text(column, row).is_some_and(|v| !v.trim().is_empty()),
            Some(Column::Number(_)) => self.number(column, row).is_some(),
            None => false,
        }
    }

    /// The interned text of a text column. A number column has no text of its
    /// own - `display` is what renders one.
    pub fn text(&self, column: usize, row: u32) -> Option<&str> {
        match self.columns.get(column)? {
            Column::Text(text) => match *text.values.get(row as usize)? {
                NO_TEXT => None,
                id => Some(&text.dict[id as usize]),
            },
            Column::Number(_) => None,
        }
    }

    pub fn number(&self, column: usize, row: u32) -> Option<f64> {
        match self.columns.get(column)? {
            Column::Number(numbers) => numbers.get(row as usize).copied().filter(|v| v.is_finite()),
            // A text column can still be aggregated when its values are numeric,
            // which is what makes a mis-typed pack field usable rather than dead
            Column::Text(_) => self.text(column, row)?.trim().parse().ok(),
        }
    }

    /// The value as the UI shows it, whatever the column holds.
    pub fn display(&self, column: usize, row: u32) -> Option<String> {
        match self.columns.get(column)? {
            Column::Text(_) => self.text(column, row).map(str::to_string),
            Column::Number(_) => self.number(column, row).map(format_number),
        }
    }
}

/// Numbers read back as text: whole values without a pointless `.0`, fractions
/// as they were measured.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Runs a pack's extractors over one line into `row`. Each regex is gated on the
/// literal it cannot match without: the substring test costs a fraction of a
/// capture run, and this runs on every hit - an unfiltered query makes a hit of
/// every line in the range.
pub fn extract_into(line: &str, fields: &[(usize, CompiledField)], row: &mut FieldRow) {
    for (column, field) in fields {
        if let Some(gate) = &field.gate {
            if !line.contains(gate.as_str()) {
                continue;
            }
        }
        let Some(caps) = field.regex.captures(line) else {
            continue;
        };
        // Named `v` if the pattern has it, else the first plain group - validated
        // when the pack compiled, so one of the two is always there
        let Some(matched) = caps.name("v").or_else(|| caps.get(1)) else {
            continue;
        };
        let text = matched.as_str();
        match field.kind {
            FieldType::Number => {
                if let Ok(value) = text.trim().parse::<f64>() {
                    if value.is_finite() {
                        row.set(*column, Extracted::Number(value));
                    }
                }
            }
            // Lowercased, so the same id written in two cases is one category in
            // a chart rather than two
            FieldType::Guid => match is_guid(text) {
                true => row.set_lowercase(*column, text),
                false => continue,
            },
            FieldType::String => row.set(*column, Extracted::Text(text)),
        }
    }
}

/// Whether the text is a hyphenated GUID. Values that are not are dropped rather
/// than stored: a pack declaring a field a GUID is saying the column can be
/// compared as one.
fn is_guid(text: &str) -> bool {
    let mut digits = 0;
    for byte in text.bytes() {
        match byte {
            b'-' => continue,
            b if b.is_ascii_hexdigit() => digits += 1,
            _ => return false,
        }
    }
    digits == 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{self, PackOrigin};

    fn metas() -> Vec<FieldMeta> {
        vec![
            FieldMeta {
                key: "p.op".into(),
                label: "Op".into(),
                kind: FieldType::String,
            },
            FieldMeta {
                key: "p.ms".into(),
                label: "Duration".into(),
                kind: FieldType::Number,
            },
        ]
    }

    fn row(op: Option<&str>, ms: Option<f64>) -> FieldRow {
        let mut row = FieldRow::new(2);
        if let Some(op) = op {
            row.set(0, Extracted::Text(op));
        }
        if let Some(ms) = ms {
            row.set(1, Extracted::Number(ms));
        }
        row
    }

    #[test]
    fn stores_and_reads_back_values_by_row() {
        let mut columns = FieldColumns::new(&metas());
        assert_eq!(columns.push_row(&row(Some("GetUser"), Some(12.5))), 0);
        assert_eq!(columns.push_row(&row(Some("PostUser"), None)), 1);

        assert_eq!(columns.rows(), 2);
        assert_eq!(columns.text(0, 0), Some("GetUser"));
        assert_eq!(columns.number(1, 0), Some(12.5));
        assert_eq!(columns.text(0, 1), Some("PostUser"));
        assert_eq!(columns.number(1, 1), None);
    }

    /// The dictionary is the point: a category repeated across rows is stored
    /// once however many rows carry it.
    #[test]
    fn interns_repeated_text_once() {
        let mut columns = FieldColumns::new(&metas());
        for _ in 0..100 {
            columns.push_row(&row(Some("GetUser"), None));
        }
        columns.push_row(&row(Some("Other"), None));

        let Column::Text(text) = &columns.columns[0] else {
            panic!("column 0 is text")
        };
        assert_eq!(text.dict.len(), 2);
        assert_eq!(columns.text(0, 50), Some("GetUser"));
        assert_eq!(columns.text(0, 100), Some("Other"));
    }

    #[test]
    fn merging_keeps_the_value_the_entry_opened_with() {
        let mut columns = FieldColumns::new(&metas());
        columns.push_row(&row(Some("GetUser"), None));
        // A later line of the same entry carries a duration and another operation
        columns.merge_into_last(&row(Some("Ignored"), Some(9.0)));

        assert_eq!(columns.rows(), 1, "merging does not add a row");
        assert_eq!(columns.text(0, 0), Some("GetUser"));
        assert_eq!(columns.number(1, 0), Some(9.0));
    }

    /// A column the result never filled in is an empty table column, and the UI
    /// hides it - which only works if emptiness is read off the rows the hits
    /// actually point at.
    #[test]
    fn reports_which_columns_carry_a_value() {
        let mut columns = FieldColumns::new(&metas());
        columns.push_row(&row(Some("GetUser"), Some(12.5)));
        columns.push_row(&row(None, None));
        // Blank is what an empty cell shows either way
        columns.push_row(&row(Some("   "), None));

        assert_eq!(columns.populated(0..3), vec![true, true]);
        assert_eq!(columns.populated(1..3), vec![false, false]);
        assert_eq!(
            columns.populated(std::iter::empty()),
            vec![false, false],
            "a result with no hits has no columns to show"
        );
    }

    /// Parallel scan units intern independently, so concatenation has to remap
    /// their dictionary indices rather than copy them.
    #[test]
    fn appending_remaps_independently_interned_text() {
        let metas = metas();
        let mut first = FieldColumns::new(&metas);
        first.push_row(&row(Some("A"), Some(1.0)));
        first.push_row(&row(Some("B"), None));

        let mut second = FieldColumns::new(&metas);
        // Reverse order, so a copied index would read back as the wrong value
        second.push_row(&row(Some("B"), None));
        second.push_row(&row(Some("C"), Some(3.0)));

        let offset = first.append(second);
        assert_eq!(offset, 2);
        assert_eq!(first.rows(), 4);
        assert_eq!(first.text(0, 2), Some("B"));
        assert_eq!(first.text(0, 3), Some("C"));
        assert_eq!(first.number(1, 3), Some(3.0));
    }

    /// Stitching two scan units can discover that their hits are one entry;
    /// the rows are already stored, so they merge in place.
    #[test]
    fn merging_two_stored_rows_keeps_the_earlier_values() {
        let mut columns = FieldColumns::new(&metas());
        columns.push_row(&row(Some("GetUser"), None));
        columns.push_row(&row(Some("Later"), Some(7.0)));

        columns.merge_rows(0, 1);
        assert_eq!(columns.text(0, 0), Some("GetUser"), "the target's value wins");
        assert_eq!(columns.number(1, 0), Some(7.0), "an absent value is filled in");
        assert_eq!(columns.rows(), 2, "the source row is left in place, unreferenced");
    }

    #[test]
    fn formats_numbers_without_a_pointless_decimal() {
        let mut columns = FieldColumns::new(&metas());
        columns.push_row(&row(None, Some(1500.0)));
        columns.push_row(&row(None, Some(12.25)));
        assert_eq!(columns.display(1, 0).as_deref(), Some("1500"));
        assert_eq!(columns.display(1, 1).as_deref(), Some("12.25"));
    }

    fn compiled(json: &str) -> Vec<(usize, CompiledField)> {
        let pack = plugin::parse_and_compile(json, PackOrigin::Bundled, false).unwrap();
        pack.fields.into_iter().enumerate().collect()
    }

    #[test]
    fn extracts_declared_fields_from_a_line() {
        let fields = compiled(
            r#"{
                "id": "p", "name": "P", "source": { "type": "kudu" },
                "fields": [
                  { "key": "op", "label": "Op", "gate": "Handling ",
                    "regex": "Handling (?<v>\\w+)" },
                  { "key": "ms", "label": "Duration", "gate": "ms", "type": "number",
                    "regex": " (?<v>\\d+(?:\\.\\d+)?)ms\\b" },
                  { "key": "login", "label": "Login", "gate": "ID_Login", "type": "guid",
                    "regex": "ID_Login[:=]'?(?<v>[0-9a-fA-F-]{36})" }
                ]
            }"#,
        );
        let mut row = FieldRow::new(3);
        extract_into(
            "13.07.2026 09:15:23.863 INFO - Handling GetUserQuery \
             ID_Login:3306928E-aaaa-bbbb-cccc-ddddeeeeffff took 192.907ms",
            &fields,
            &mut row,
        );

        assert_eq!(row.get(0), Some(Extracted::Text("GetUserQuery")));
        assert_eq!(row.get(1), Some(Extracted::Number(192.907)));
        assert_eq!(
            row.get(2),
            Some(Extracted::Text("3306928e-aaaa-bbbb-cccc-ddddeeeeffff")),
            "a GUID is stored lowercased so its case cannot split a category"
        );
    }

    /// The gate is the reason extraction is affordable: a line without the
    /// literal never reaches the regex.
    #[test]
    fn a_line_missing_the_gate_extracts_nothing() {
        let fields = compiled(
            r#"{
                "id": "p", "name": "P", "source": { "type": "kudu" },
                "fields": [ { "key": "op", "label": "Op", "gate": "Handling ",
                              "regex": "Handling (?<v>\\w+)" } ]
            }"#,
        );
        let mut row = FieldRow::new(1);
        extract_into("13.07.2026 09:15:23.863 INFO - Request starting", &fields, &mut row);
        assert!(row.is_empty());
    }

    #[test]
    fn drops_a_guid_field_whose_value_is_not_one() {
        let fields = compiled(
            r#"{
                "id": "p", "name": "P", "source": { "type": "kudu" },
                "fields": [ { "key": "login", "label": "Login", "type": "guid",
                              "regex": "ID_Login[:=](?<v>\\S+)" } ]
            }"#,
        );
        let mut row = FieldRow::new(1);
        extract_into("INFO - ID_Login:not-a-guid", &fields, &mut row);
        assert!(row.is_empty(), "a malformed GUID is not a GUID value");
    }

    #[test]
    fn a_number_field_ignores_a_value_that_is_not_finite() {
        let fields = compiled(
            r#"{
                "id": "p", "name": "P", "source": { "type": "kudu" },
                "fields": [ { "key": "n", "label": "N", "type": "number",
                              "regex": "took (?<v>\\S+)" } ]
            }"#,
        );
        let mut row = FieldRow::new(1);
        extract_into("took NaN", &fields, &mut row);
        assert!(row.is_empty());
        extract_into("took inf", &fields, &mut row);
        assert!(row.is_empty());
    }

    /// A pack author who types `"type": "string"` on a numeric field should still
    /// get a chartable column rather than a dead one.
    #[test]
    fn a_numeric_text_column_can_still_be_aggregated() {
        let mut columns = FieldColumns::new(&[FieldMeta {
            key: "p.n".into(),
            label: "N".into(),
            kind: FieldType::String,
        }]);
        let mut r = FieldRow::new(1);
        r.set(0, Extracted::Text("42.5"));
        columns.push_row(&r);
        assert_eq!(columns.number(0, 0), Some(42.5));
    }
}
