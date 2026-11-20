use crate::drives;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout},
    style::Color,
    text::Line,
    widgets::{Block, BorderType, Padding, Paragraph, Row, Table, TableState, Widget, Wrap},
    Frame,
};

pub enum InputResult {
    None,
    Quit,
    QuitChangeDirectory(String),
}

#[derive(Default)]
pub struct Tui {
    ts: TableState,
    pub drv: Vec<drives::Drive>,
    selected: Option<drives::Block>,
    pub last_status: String,
    scan_error: Option<String>,
}

const HELP: &str = "j/▼⋮k/▲⋮l/o/▶ - CD⋮m - Mount⋮u - Unmount⋮e - Eject⋮q - Quit";

impl Tui {
    pub fn update(&mut self, scan: &Result<Vec<drives::Drive>, String>) {
        match scan {
            Ok(drives) => {
                self.drv.clone_from(drives);
                self.scan_error = None;
            }
            Err(error) => self.scan_error = Some(error.clone()),
        }
    }

    fn set_status(&mut self, res: udisks2::Result<()>) {
        self.last_status = match res {
            Ok(()) => String::from("Ok"),
            Err(e) => format!("Error: {e:?}"),
        }
    }

    pub async fn input(&mut self, key: KeyEvent) -> InputResult {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.ts.select_previous();
                self.selected = None;
                InputResult::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.ts.select_next();
                self.selected = None;
                InputResult::None
            }
            KeyCode::Char('m') => {
                if let Some(b) = &self.selected {
                    self.set_status(drives::mount(b).await);
                }
                InputResult::None
            }
            KeyCode::Char('u') => {
                if let Some(b) = &self.selected {
                    self.set_status(drives::unmount(b).await);
                }
                InputResult::None
            }
            KeyCode::Char('e') => {
                if let Some(b) = &self.selected {
                    self.set_status(drives::eject(b).await);
                }
                InputResult::None
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l' | 'o') => {
                if let Some(s) = &self.selected {
                    let output = s.clone().mount.unwrap_or_default();
                    InputResult::QuitChangeDirectory(output)
                } else {
                    InputResult::Quit
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => InputResult::Quit,
            _ => InputResult::None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let layout = Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints(vec![Constraint::Fill(1), Constraint::Length(5)])
            .split(frame.area());

        let block = Block::bordered()
            .title(Line::styled("Mount", Color::White))
            .title_alignment(Alignment::Center)
            .padding(Padding::symmetric(1, 1))
            .border_type(BorderType::Rounded)
            .border_style(Color::Yellow);
        block.clone().render(layout[0], frame.buffer_mut());

        let rows: Vec<drives::Block> = self.drv.iter().flat_map(|d| d.blocks.clone()).collect();

        if let Some(row) = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| same_device(row, selected)))
        {
            self.ts.select(Some(row));
        }

        if self.ts.selected().is_none() && !self.drv.is_empty() {
            self.ts.select(Some(0));
        }

        self.ts
            .selected()
            .and_then(|n| rows.get(n).cloned())
            .clone_into(&mut self.selected);

        let max_size_field_length: u16 = rows
            .iter()
            .map(|r| r.size.len())
            .max()
            .unwrap_or(0)
            .try_into()
            .unwrap_or(0);

        let rows = rows.iter().map(|i| {
            Row::new(vec![
                i.dev.clone(),
                i.label.clone(),
                i.mount.clone().unwrap_or_default(),
                i.size.clone(),
                if i.mounted {
                    "M".to_owned()
                } else {
                    "O".to_owned()
                },
            ])
        });
        let widths = [
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Length(max_size_field_length),
            Constraint::Length(1),
        ];
        let table = Table::new(rows, widths)
            .row_highlight_style(Color::Green)
            .highlight_symbol(">");

        frame.render_stateful_widget(table, block.inner(layout[0]), &mut self.ts);

        let descr = match &self.selected {
            Some(s) => {
                let mnt_point = match &s.mount {
                    None => "",
                    Some(m) => m,
                };
                let mounted = if s.mounted {
                    format!("mounted to {mnt_point:?}")
                } else {
                    format!("not mounted {mnt_point:?}")
                };

                format!(
                    "dev: {:?} label: {:?} type: {:?} size {} {mounted}",
                    s.dev, s.label, s.fstype, s.size
                )
            }
            None => String::new(),
        };

        let status = match &self.scan_error {
            Some(error) => format!("Error: {error}"),
            None => self.last_status.clone(),
        };
        let info = format!("{status} | {HELP}\n{descr}");
        let info = Paragraph::new(info).wrap(Wrap { trim: true });
        frame.render_widget(info, layout[1]);
    }
}

fn same_device(first: &drives::Block, second: &drives::Block) -> bool {
    // Rows from /proc/self/mounts and fstab have no udisks object path.
    if first.object_path.is_empty() {
        first.dev == second.dev && first.mount == second.mount
    } else {
        first.object_path == second.object_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn block(object_path: &str, dev: &str, mount: Option<&str>) -> drives::Block {
        drives::Block {
            object_path: object_path.to_owned(),
            drive_path: String::new(),
            dev: dev.to_owned(),
            label: String::new(),
            mount: mount.map(str::to_owned),
            fstype: String::new(),
            mounted: mount.is_some(),
            size: String::new(),
        }
    }

    fn udisks_block(dev: &str, mount: Option<&str>) -> drives::Block {
        let object_path = dev.replace("/dev/", "/org/freedesktop/UDisks2/block_devices/");
        block(&object_path, dev, mount)
    }

    fn fstab_block(dev: &str, mount: &str) -> drives::Block {
        block("", dev, Some(mount))
    }

    fn drive(blocks: Vec<drives::Block>) -> drives::Drive {
        drives::Drive {
            id: String::new(),
            object_path: String::new(),
            model: String::new(),
            ejectable: false,
            blocks,
        }
    }

    fn draw(tui: &mut Tui) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| tui.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width.into())
            .map(|line| {
                line.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn show(tui: &mut Tui, blocks: Vec<drives::Block>) {
        tui.drv = vec![drive(blocks)];
        draw(tui);
    }

    fn selected(tui: &Tui) -> &drives::Block {
        tui.selected.as_ref().unwrap()
    }

    // A device can appear while the user looks at the list. `m`, `u` and `e` act on the selected row,
    // so the cursor must stay on the device that the user selected, also when that device gets mounted.
    #[tokio::test]
    async fn selection_stays_on_device_when_rows_move() {
        let mut tui = Tui::default();
        let rows = vec![
            udisks_block("/dev/sdb1", None),
            udisks_block("/dev/sdc1", None),
        ];
        show(&mut tui, rows.clone());
        tui.input(KeyCode::Down.into()).await;
        show(&mut tui, rows);

        let new_device = udisks_block("/dev/sda1", None);
        let rows = vec![
            new_device,
            udisks_block("/dev/sdb1", None),
            udisks_block("/dev/sdc1", Some("/mnt/c")),
        ];
        show(&mut tui, rows);
        assert_eq!(selected(&tui).dev, "/dev/sdc1");
    }

    // The cursor follows the device from one frame to the next, but a key must still move it.
    #[tokio::test]
    async fn down_key_moves_to_next_device() {
        let mut tui = Tui::default();
        let blocks = vec![
            udisks_block("/dev/sda1", None),
            udisks_block("/dev/sdb1", None),
        ];
        show(&mut tui, blocks.clone());
        tui.input(KeyCode::Down.into()).await;
        show(&mut tui, blocks);
        assert_eq!(selected(&tui).dev, "/dev/sdb1");
    }

    // fstab can have many lines with the same dev, for example btrfs subvolumes.
    // These rows have no udisks object path, so only the mount path tells them apart.
    #[tokio::test]
    async fn fstab_rows_with_same_dev_stay_apart() {
        let mut tui = Tui::default();
        let subvolumes = vec![
            fstab_block("UUID=1234", "/home"),
            fstab_block("UUID=1234", "/var/log"),
        ];
        show(&mut tui, subvolumes.clone());
        tui.input(KeyCode::Down.into()).await;
        show(&mut tui, subvolumes.clone());

        show(
            &mut tui,
            [vec![udisks_block("/dev/sda1", None)], subvolumes].concat(),
        );
        assert_eq!(selected(&tui).mount.as_deref(), Some("/var/log"));
    }

    // udisks can stop or fail. The user must see why the list is empty or old.
    #[test]
    fn scan_error_shows_until_next_good_scan() {
        let mut tui = Tui::default();
        let good_scan = Ok(vec![drive(vec![udisks_block("/dev/sda1", None)])]);
        tui.update(&good_scan);
        tui.update(&Err("udisks is not running".to_owned()));
        let screen = draw(&mut tui);
        assert!(screen.contains("Error: udisks is not running"));
        assert!(screen.contains("/dev/sda1"));

        tui.update(&good_scan);
        assert!(!draw(&mut tui).contains("Error"));
    }

    // The help line shows ▶ for CD.
    #[tokio::test]
    async fn right_key_changes_directory() {
        let mut tui = Tui::default();
        show(&mut tui, vec![udisks_block("/dev/sda1", Some("/mnt/a"))]);
        let result = tui.input(KeyCode::Right.into()).await;
        assert!(matches!(result, InputResult::QuitChangeDirectory(path) if path == "/mnt/a"));
    }
}
