//! The snapshot a client attaching to a terminal starts from: ANSI bytes that,
//! written into a fresh emulator of the same size, draw the host's screen
//! again. Scrollback first (up to [`SCROLLBACK_LINES`]), then the visible grid,
//! then the state later output depends on: the alternate screen, the cursor
//! (and the one DECSC saved), its pen and character sets, the scrolling
//! region and the terminal modes.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    CharsetIndex, Color, CursorShape, CursorStyle, Handler, NamedColor, NamedPrivateMode,
    PrivateMode, StandardCharset,
};

/// Scrollback lines a snapshot carries, and all the host's emulator keeps.
pub const SCROLLBACK_LINES: usize = 2000;

/// Attributes SGR sets; the rest of a cell's flags are its layout.
const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The scrolling region (DECSTBM), which `Term` keeps to itself: a second
/// parser fed the same bytes follows it the way `Term` does.
pub struct ScrollRegion {
    rows: usize,
    /// First line, 0-based.
    top: i32,
    /// One past the last line.
    end: i32,
}

impl ScrollRegion {
    pub fn new(rows: usize) -> Self {
        Self {
            rows,
            top: 0,
            end: rows as i32,
        }
    }

    /// A resize resets the region, as `Term::resize` does.
    pub fn resize(&mut self, rows: usize) {
        *self = Self::new(rows);
    }

    fn is_full(&self) -> bool {
        self.top == 0 && self.end == self.rows as i32
    }
}

impl Handler for ScrollRegion {
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let bottom = bottom.unwrap_or(self.rows);
        if top >= bottom {
            return;
        }
        let rows = self.rows as i32;
        self.top = (top as i32 - 1).min(rows);
        self.end = (bottom as i32).min(rows);
    }

    fn reset_state(&mut self) {
        self.resize(self.rows);
    }

    // DECCOLM resets the region whichever way it's set.
    fn set_private_mode(&mut self, mode: PrivateMode) {
        if mode == PrivateMode::Named(NamedPrivateMode::ColumnMode) {
            self.resize(self.rows);
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        self.set_private_mode(mode);
    }
}

/// What a cell's SGR sets.
#[derive(Clone, PartialEq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
    underline: Option<Color>,
}

impl Default for Pen {
    fn default() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
            underline: None,
        }
    }
}

impl Pen {
    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & SGR_FLAGS,
            underline: cell.underline_color(),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "4:2"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if self.flags.contains(flag) {
                out.push(b';');
                out.extend_from_slice(code.as_bytes());
            }
        }
        color(out, self.fg, 30, 90, 38);
        color(out, self.bg, 40, 100, 48);
        if let Some(underline) = self.underline {
            color(out, underline, 0, 0, 58);
        }
        out.push(b'm');
    }
}

/// One color's SGR parameters: `base`/`bright` for the 16 named colors (0
/// for the underline, which has no short form), `extended` for 256 and RGB.
/// The default colors need nothing after the `0` that starts every pen.
fn color(out: &mut Vec<u8>, color: Color, base: u8, bright: u8, extended: u8) {
    let text = match color {
        Color::Named(named) => {
            let index = named as usize;
            match index {
                0..=7 if base != 0 => format!(";{}", base as usize + index),
                8..=15 if base != 0 => format!(";{}", bright as usize + index - 8),
                0..=15 => format!(";{extended};5;{index}"),
                _ => return,
            }
        }
        Color::Indexed(index) => format!(";{extended};5;{index}"),
        Color::Spec(rgb) => format!(";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b),
    };
    out.extend_from_slice(text.as_bytes());
}

/// The bytes so far and what the receiving emulator's pen is after them.
struct Writer {
    out: Vec<u8>,
    pen: Pen,
    link: Option<Hyperlink>,
    charsets: [StandardCharset; 4],
}

const CHARSETS: [(CharsetIndex, u8); 4] = [
    (CharsetIndex::G0, b'('),
    (CharsetIndex::G1, b')'),
    (CharsetIndex::G2, b'*'),
    (CharsetIndex::G3, b'+'),
];

impl Writer {
    fn set_pen(&mut self, pen: &Pen) {
        if *pen != self.pen {
            pen.write(&mut self.out);
            self.pen = pen.clone();
        }
    }

    fn set_link(&mut self, link: Option<Hyperlink>) {
        if link == self.link {
            return;
        }
        match &link {
            Some(link) => {
                let text = format!("\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
                self.out.extend_from_slice(text.as_bytes());
            }
            None => self.out.extend_from_slice(b"\x1b]8;;\x1b\\"),
        }
        self.link = link;
    }

    /// The cursor's pen, hyperlink and character sets.
    fn set_cursor_state(&mut self, cursor: &Cursor<Cell>) {
        self.set_pen(&Pen::of(&cursor.template));
        self.set_link(cursor.template.hyperlink());
        let charsets = CHARSETS.map(|(index, _)| cursor.charsets[index]);
        self.set_charsets(charsets);
    }

    fn set_charsets(&mut self, charsets: [StandardCharset; 4]) {
        for (i, (_, intro)) in CHARSETS.iter().enumerate() {
            if charsets[i] != self.charsets[i] {
                let set = match charsets[i] {
                    StandardCharset::Ascii => b'B',
                    StandardCharset::SpecialCharacterAndLineDrawing => b'0',
                };
                self.out.extend_from_slice(&[0x1b, *intro, set]);
            }
        }
        self.charsets = charsets;
    }

    fn reset_pen(&mut self) {
        self.set_pen(&Pen::default());
        self.set_link(None);
    }

    fn cell(&mut self, cell: &Cell) {
        self.set_pen(&Pen::of(cell));
        self.set_link(cell.hyperlink());
        let mut buf = [0; 4];
        self.out
            .extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
        for c in cell.zerowidth().unwrap_or_default() {
            self.out
                .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }

    fn goto(&mut self, line: i32, column: usize) {
        let text = format!("\x1b[{};{}H", line + 1, column + 1);
        self.out.extend_from_slice(text.as_bytes());
    }

    /// Lines `from..to` of `grid` (negative lines are scrollback), drawn from
    /// the cursor's line down, scrolling as a terminal would: a soft-wrapped
    /// line runs into the next, which wraps it again.
    fn lines(&mut self, grid: &Grid<Cell>, from: i32, to: i32) {
        let columns = grid.columns();
        let last = Column(columns - 1);
        let wraps = |line: i32| grid[Line(line)][last].flags.contains(Flags::WRAPLINE);
        for line in from..to {
            let row = &grid[Line(line)];
            let continues = line > from && wraps(line - 1);
            if line > from && !continues {
                self.reset_pen();
                self.out.extend_from_slice(b"\r\n");
            }
            // A wrapped line is drawn whole so the next character wraps.
            let wrapped = line + 1 < to && wraps(line);
            let end = if wrapped {
                columns
            } else {
                (0..columns)
                    .rev()
                    .find(|&c| row[Column(c)] != Cell::default())
                    .map_or(0, |c| c + 1)
            };
            for column in 0..end {
                let cell = &row[Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                // Writing the wide character that didn't fit makes the spacer
                // again, as long as the next line starts with it.
                if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                    && wrapped
                    && grid[Line(line + 1)][Column(0)]
                        .flags
                        .contains(Flags::WIDE_CHAR)
                {
                    continue;
                }
                self.cell(cell);
            }
            if end < columns {
                // An empty continuation still needs a character to wrap to it.
                if continues && end == 0 {
                    self.reset_pen();
                    self.out.push(b' ');
                }
                // Lines scrolled in take the pen's background: clear the rest.
                self.reset_pen();
                self.out.extend_from_slice(b"\x1b[K");
            }
        }
    }

    /// Moves to the cursor, waiting to wrap if it was (a character written
    /// in the last column again), and takes its pen and character sets.
    fn cursor(&mut self, grid: &Grid<Cell>, cursor: &Cursor<Cell>, top: i32) {
        let line = cursor.point.line.0;
        let last = grid.columns() - 1;
        if cursor.input_needs_wrap {
            let mut column = last;
            if grid[Line(line)][Column(column)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
                && column > 0
            {
                column -= 1;
            }
            self.goto(line - top, column);
            let mut cell = grid[Line(line)][Column(column)].clone();
            if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                cell.c = ' ';
            }
            self.cell(&cell);
        } else {
            self.goto(line - top, cursor.point.column.0);
        }
        self.set_cursor_state(cursor);
    }
}

/// The snapshot of `term`. Needs `&mut` only to look at the primary screen
/// under the alternate one (`Term` keeps it to itself); `term` is left as it
/// was.
pub fn snapshot<T: EventListener>(
    term: &mut Term<T>,
    region: &ScrollRegion,
    title: Option<&str>,
) -> Vec<u8> {
    let mut w = Writer {
        out: b"\x1bc".to_vec(),
        pen: Pen::default(),
        link: None,
        charsets: Default::default(),
    };
    let mode = *term.mode();
    let rows = term.screen_lines() as i32;

    if mode.contains(TermMode::ALT_SCREEN) {
        // `swap_alt` back resets the alternate grid and its cursor: keep them.
        let alt = term.grid().clone();
        term.swap_alt();
        primary(&mut w, term.grid());
        term.swap_alt();
        *term.grid_mut() = alt;
        w.out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        w.set_charsets(Default::default());
        w.lines(term.grid(), 0, rows);
    } else {
        let grid = term.grid();
        let history = grid.history_size().min(SCROLLBACK_LINES) as i32;
        w.lines(grid, -history, rows);
    }

    let grid = term.grid();
    // The cursor DECSC saved, then everything that moves the cursor home.
    w.goto(
        grid.saved_cursor.point.line.0,
        grid.saved_cursor.point.column.0,
    );
    w.set_cursor_state(&grid.saved_cursor);
    w.out.extend_from_slice(b"\x1b7");
    if !region.is_full() {
        let text = format!("\x1b[{};{}r", region.top + 1, region.end);
        w.out.extend_from_slice(text.as_bytes());
    }
    let top = if mode.contains(TermMode::ORIGIN) {
        w.out.extend_from_slice(b"\x1b[?6h");
        region.top
    } else {
        0
    };
    w.cursor(grid, &grid.cursor, top);

    let default = TermMode::default();
    for (flag, set, unset) in [
        (TermMode::APP_CURSOR, "\x1b[?1h", ""),
        (TermMode::APP_KEYPAD, "\x1b=", ""),
        (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", ""),
        (TermMode::MOUSE_DRAG, "\x1b[?1002h", ""),
        (TermMode::MOUSE_MOTION, "\x1b[?1003h", ""),
        (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", ""),
        (TermMode::UTF8_MOUSE, "\x1b[?1005h", ""),
        (TermMode::SGR_MOUSE, "\x1b[?1006h", ""),
        (TermMode::BRACKETED_PASTE, "\x1b[?2004h", ""),
        (TermMode::INSERT, "\x1b[4h", ""),
        (TermMode::LINE_FEED_NEW_LINE, "\x1b[20h", ""),
        (TermMode::LINE_WRAP, "", "\x1b[?7l"),
        (TermMode::ALTERNATE_SCROLL, "", "\x1b[?1007l"),
        (TermMode::URGENCY_HINTS, "", "\x1b[?1042l"),
    ] {
        if mode.contains(flag) != default.contains(flag) {
            let text = if mode.contains(flag) { set } else { unset };
            w.out.extend_from_slice(text.as_bytes());
        }
    }

    let style = term.cursor_style();
    if style != CursorStyle::default() {
        let shape = match style.shape {
            CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 1,
            CursorShape::Underline => 3,
            CursorShape::Beam => 5,
        };
        let text = format!("\x1b[{} q", shape + u8::from(!style.blinking));
        w.out.extend_from_slice(text.as_bytes());
    }
    if !mode.contains(TermMode::SHOW_CURSOR) {
        w.out.extend_from_slice(b"\x1b[?25l");
    }
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        w.out.extend_from_slice(b"\x1b]2;");
        let title: String = title.chars().filter(|c| !c.is_control()).collect();
        w.out.extend_from_slice(title.as_bytes());
        w.out.push(0x07);
    }
    w.out
}

/// The primary screen under the alternate one: its scrollback and grid, and
/// its cursor, which `?1049h` saves and leaving the alternate screen brings
/// back.
fn primary(w: &mut Writer, grid: &Grid<Cell>) {
    let history = grid.history_size().min(SCROLLBACK_LINES) as i32;
    w.lines(grid, -history, grid.screen_lines() as i32);
    w.cursor(grid, &grid.cursor, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    /// A host's emulator (with its region tracker) and a client's.
    struct Pair {
        host: Term<VoidListener>,
        host_parser: Processor,
        region: ScrollRegion,
        region_parser: Processor,
        size: (usize, usize),
    }

    impl Pair {
        fn new(columns: usize, rows: usize) -> Self {
            let config = Config {
                scrolling_history: SCROLLBACK_LINES,
                ..Config::default()
            };
            Self {
                host: Term::new(config, &Size(columns, rows), VoidListener),
                host_parser: Processor::new(),
                region: ScrollRegion::new(rows),
                region_parser: Processor::new(),
                size: (columns, rows),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.host_parser.advance(&mut self.host, bytes);
            self.region_parser.advance(&mut self.region, bytes);
        }

        /// A fresh client fed the snapshot.
        fn client(&mut self) -> (Term<VoidListener>, Processor) {
            let bytes = snapshot(&mut self.host, &self.region, None);
            let mut client = Term::new(
                Config::default(),
                &Size(self.size.0, self.size.1),
                VoidListener,
            );
            let mut parser = Processor::new();
            parser.advance(&mut client, &bytes);
            (client, parser)
        }
    }

    fn assert_same(a: &Term<VoidListener>, b: &Term<VoidListener>) {
        assert_eq!(a.mode(), b.mode(), "modes");
        assert_eq!(a.cursor_style(), b.cursor_style(), "cursor style");
        let (ga, gb) = (a.grid(), b.grid());
        assert_eq!(ga.history_size(), gb.history_size(), "scrollback");
        for line in -(ga.history_size() as i32)..ga.screen_lines() as i32 {
            for column in 0..ga.columns() {
                let (ca, cb) = (
                    &ga[Line(line)][Column(column)],
                    &gb[Line(line)][Column(column)],
                );
                assert_eq!(ca, cb, "cell at line {line}, column {column}");
            }
        }
        for (name, ca, cb) in [
            ("cursor", &ga.cursor, &gb.cursor),
            ("saved cursor", &ga.saved_cursor, &gb.saved_cursor),
        ] {
            assert_eq!(ca.point, cb.point, "{name} position");
            assert_eq!(ca.template, cb.template, "{name} pen");
            assert_eq!(ca.charsets, cb.charsets, "{name} character sets");
            assert_eq!(
                ca.input_needs_wrap, cb.input_needs_wrap,
                "{name} pending wrap"
            );
        }
    }

    /// Feeds `setup` to a host, checks a client drawn from its snapshot is
    /// the same, then feeds both `after` (which depends on state beyond the
    /// grid) and checks again.
    fn round_trip(columns: usize, rows: usize, setup: &[u8], after: &[u8]) {
        let mut pair = Pair::new(columns, rows);
        pair.feed(setup);
        let (mut client, mut parser) = pair.client();
        assert_same(&pair.host, &client);
        pair.feed(after);
        parser.advance(&mut client, after);
        assert_same(&pair.host, &client);
    }

    #[test]
    fn colors_and_attributes() {
        round_trip(
            40,
            8,
            b"plain \x1b[1;31mbold red\x1b[0m \x1b[2;3;4mdim italic\x1b[0m\r\n\
              \x1b[38;5;202;48;5;17mindexed\x1b[0m \x1b[38;2;10;20;30;48;2;200;100;50mrgb\x1b[0m\r\n\
              \x1b[7minverse\x1b[27m \x1b[9mstrike\x1b[0m \x1b[4:3;58;2;255;0;0mcurl\x1b[0m \x1b[21mdouble\x1b[0m\r\n\
              \x1b[95;104mbright\x1b[0m \x1b[8mhidden\x1b[0m\r\n\
              \x1b[42m\x1b[Kgreen to the end\x1b[0m\r\n\
              \x1b[2;5H\x1b[36;1m\x1b7\x1b[0m\x1b[6;1H\
              \x1b[1;34mpen stays",
            b" and continues\x1b[0m done\x1b8restored",
        );
    }

    #[test]
    fn scrollback_and_wrapped_lines() {
        let mut setup = Vec::new();
        for i in 0..60 {
            setup.extend_from_slice(format!("line {i} \x1b[3{}m", i % 8).as_bytes());
            if i % 7 == 0 {
                // Wraps over three lines, the last one starting colored.
                setup.extend_from_slice(&[b'w'; 40]);
                setup.extend_from_slice(b"\x1b[41mred\x1b[0m");
            }
            setup.extend_from_slice(b"\x1b[0m\r\n");
        }
        // Wrapped on the top line, so without scrolling: drawn at the bottom
        // instead, the colored pen mustn't color the rest of the line.
        setup.extend_from_slice(b"\x1b[2J\x1b[H");
        setup.extend_from_slice(&[b'v'; 20]);
        setup.extend_from_slice(b"\x1b[41mred\x1b[0m\r\n");
        // A wrap whose continuation is empty, then the pending wrap itself.
        setup.extend_from_slice(&[b'x'; 40]);
        setup.extend_from_slice(b" \r\n");
        setup.extend_from_slice(&[b'y'; 20]);
        round_trip(20, 6, &setup, b"z\r\nnext");
    }

    #[test]
    fn wide_characters() {
        round_trip(
            11,
            5,
            "漢字 wide\r\n\u{1b}[33m0123456789漢 wraps\u{1b}[0m\r\ne\u{301}combining\r\n0123456789"
                .as_bytes(),
            "字".as_bytes(),
        );
    }

    #[test]
    fn alternate_screen_box_and_cursor() {
        let mut setup = b"primary $ \x1b[1mls\x1b[0m\r\nout\r\n$ ".to_vec();
        // Into the alternate screen: a box, a scrolling region, a pen, a
        // saved cursor, line drawing and modes.
        setup.extend_from_slice(b"\x1b(0\x1b[?1049h\x1b[2J\x1b[H");
        setup.extend_from_slice(b"\x1b(0lqqqqqqqqk\r\n");
        for _ in 0..3 {
            setup.extend_from_slice(b"x\x1b[8Cx\r\n");
        }
        setup.extend_from_slice(b"mqqqqqqqqj\x1b(B");
        setup.extend_from_slice(b"\x1b[3;6H\x1b[44;97mhi\x1b[0m");
        setup.extend_from_slice(b"\x1b[2;3H\x1b[35m\x1b7\x1b[0m");
        setup.extend_from_slice(b"\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h");
        setup.extend_from_slice(b"\x1b[6 q\x1b[?25l\x1b[2;7r\x1b[7;12H\x1b[32m\x1b(0");
        round_trip(
            20,
            8,
            &setup,
            // Scrolls the region, restores the saved cursor, leaves the
            // alternate screen and writes on the primary one.
            b"q\n\n\nq\x1b8again\x1b[?1049lback\x1b[?25h",
        );
    }

    #[test]
    fn origin_mode_and_modes_off() {
        round_trip(
            15,
            6,
            b"top\r\n\x1b[2;5r\x1b[?6h\x1b[2;3Hin\x1b[?7l\x1b[4h\x1b[20h\x1b[?1007l",
            b"\x1b[Hab\x1b[3;1Hlong line past the edge\nnl",
        );
    }

    #[test]
    fn hyperlinks_and_title() {
        let mut pair = Pair::new(30, 4);
        pair.feed(b"\x1b]8;id=x;https://example.com\x1b\\link\x1b]8;;\x1b\\ text");
        let (client, _) = pair.client();
        assert_same(&pair.host, &client);
        let bytes = snapshot(&mut pair.host, &pair.region, Some("my\x07title"));
        assert!(bytes.ends_with(b"\x1b]2;mytitle\x07"));
    }
}
