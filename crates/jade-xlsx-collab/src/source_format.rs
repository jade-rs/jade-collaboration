//! Editable base styles, independent of conditional formatting. Keep this
//! projection aligned with the editor's format_at; fixture tests compare
//! every returned attribute, including unresolved/automatic colours.
use crate::{BorderLine, BorderStyle, FormatChange};
use jade_xlsx_reader::{BorderEdge, Color, StyleTable};

fn color(color: Color) -> Option<String> {
    jade_xlsx_layout::color::rgba(color)
        .filter(|rgba| rgba[3] == 255)
        .map(|rgba| format!("{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]))
}

fn border(edge: Option<&BorderEdge>) -> BorderLine {
    let Some(edge) = edge else {
        return BorderLine::none();
    };
    let Some(style) = BorderStyle::parse(&edge.style) else {
        return BorderLine::none();
    };
    let color = edge.color.and_then(|c| match c {
        Color::Auto | Color::Indexed(64) => None,
        c => color(c),
    });
    BorderLine {
        style,
        color: color.filter(|_| style != BorderStyle::None),
    }
}

pub(crate) fn read(styles: &StyleTable, index: u32) -> FormatChange {
    let format = styles.cell_format(Some(index));
    let font = format.and_then(|f| styles.font(f));
    let fill = format.and_then(|f| styles.fill(f));
    let edges = format.and_then(|f| styles.border(f));
    let alignment = format.map(|f| f.alignment).unwrap_or_default();
    FormatChange {
        bold: Some(font.is_some_and(|f| f.bold)),
        italic: Some(font.is_some_and(|f| f.italic)),
        underline: Some(font.is_some_and(|f| f.underline)),
        strike: Some(font.is_some_and(|f| f.strike)),
        font_name: font.and_then(|f| f.name.clone()),
        font_size: font.and_then(|f| f.size_points),
        font_color: font.and_then(|f| f.color).and_then(color),
        fill_color: match fill.and_then(|f| f.pattern.as_deref()) {
            None | Some("none") => Some(String::new()),
            Some("solid") => fill.and_then(|f| f.foreground).and_then(color),
            _ => None,
        },
        number_format: match format {
            None => Some("General".into()),
            Some(f) if f.number_format_id.is_none() => Some("General".into()),
            Some(f) => styles.number_format_code(f).map(str::to_owned),
        },
        horizontal: Some(alignment.horizontal),
        vertical: Some(alignment.vertical),
        wrap_text: Some(alignment.wrap_text),
        border_left: Some(border(edges.and_then(|b| b.left.as_ref()))),
        border_right: Some(border(edges.and_then(|b| b.right.as_ref()))),
        border_top: Some(border(edges.and_then(|b| b.top.as_ref()))),
        border_bottom: Some(border(edges.and_then(|b| b.bottom.as_ref()))),
        border_diagonal: Some(border(edges.and_then(|b| b.diagonal.as_ref()))),
        diagonal_up: Some(edges.is_some_and(|b| b.diagonal_up)),
        diagonal_down: Some(edges.is_some_and(|b| b.diagonal_down)),
    }
}
