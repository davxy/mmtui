use crate::drives;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout},
    style::Color,
    text::Line,
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Row, Table, TableState, Widget, Wrap},
    Frame,
};
use std::future::Future;
use tokio::task::JoinHandle;

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
    passphrase_prompt: Option<PassphrasePrompt>,
    operation: Option<JoinHandle<udisks2::Result<()>>>,
}

struct PassphrasePrompt {
    block: drives::Block,
    passphrase: String,
}

const HELP: &str = "j/▼⋮k/▲⋮l/o/▶ - CD⋮m - Mount⋮u - Unmount⋮e - Eject⋮q - Quit";
const PROMPT_HELP: &str = "Enter - Unlock⋮Esc - Cancel";
const MAX_MOUNT_TEXT_LENGTH: usize = 30;

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
            Err(e) => format!("Error: {e}"),
        }
    }

    fn start(
        &mut self,
        progress: &str,
        operation: impl Future<Output = udisks2::Result<()>> + Send + 'static,
    ) {
        progress.clone_into(&mut self.last_status);
        self.operation = Some(tokio::spawn(operation));
    }

    pub async fn finish_operation(&mut self) {
        if let Some(operation) = self.operation.take_if(|operation| operation.is_finished()) {
            match operation.await {
                Ok(result) => self.set_status(result),
                Err(error) => self.last_status = format!("Error: {error}"),
            }
        }
    }

    pub fn input(&mut self, key: KeyEvent) -> InputResult {
        if let Some(prompt) = &mut self.passphrase_prompt {
            match key.code {
                KeyCode::Char(character) => prompt.passphrase.push(character),
                KeyCode::Backspace => {
                    prompt.passphrase.pop();
                }
                KeyCode::Esc => self.passphrase_prompt = None,
                KeyCode::Enter => {
                    if let Some(prompt) = self.passphrase_prompt.take() {
                        self.start("Unlocking...", async move {
                            drives::unlock_and_mount(&prompt.block, &prompt.passphrase).await
                        });
                    }
                }
                _ => {}
            }
            return InputResult::None;
        }

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
            KeyCode::Char('m' | 'u' | 'e') if self.operation.is_some() => InputResult::None,
            KeyCode::Char('m') => {
                if let Some(b) = self.selected.clone() {
                    if let drives::Encryption::Locked = b.encryption {
                        self.passphrase_prompt = Some(PassphrasePrompt {
                            block: b,
                            passphrase: String::new(),
                        });
                    } else {
                        self.start("Mounting...", async move { drives::mount(&b).await });
                    }
                }
                InputResult::None
            }
            KeyCode::Char('u') => {
                if let Some(b) = self.selected.clone() {
                    self.start("Unmounting...", async move { drives::unmount(&b).await });
                }
                InputResult::None
            }
            KeyCode::Char('e') => {
                if let Some(b) = self.selected.clone() {
                    self.start("Ejecting...", async move { drives::eject(&b).await });
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

        let max_field_length = |field_length: fn(&drives::Block) -> usize| -> u16 {
            rows.iter()
                .map(field_length)
                .max()
                .unwrap_or(0)
                .try_into()
                .unwrap_or(0)
        };
        let table_area = block.inner(layout[0]);
        let max_dev_field_length = max_field_length(|r| r.dev.len());
        let max_label_field_length = max_field_length(|r| r.label.len());
        let max_fstype_field_length = max_field_length(|r| r.fstype.len());
        let max_mount_field_length = max_field_length(|r| short_mount_path(r).chars().count());
        let max_size_field_length = max_field_length(|r| r.size.len());

        let rows = rows.iter().map(|i| {
            Row::new(vec![
                i.dev.clone(),
                i.label.clone(),
                i.fstype.clone(),
                short_mount_path(i),
                i.size.clone(),
                if i.mounted {
                    "M".to_owned()
                } else {
                    "O".to_owned()
                },
            ])
        });
        let widths = [
            Constraint::Fill(max_dev_field_length.min(table_area.width / 3)),
            Constraint::Fill(max_label_field_length),
            Constraint::Fill(max_fstype_field_length),
            Constraint::Fill(max_mount_field_length),
            Constraint::Length(max_size_field_length),
            Constraint::Length(1),
        ];
        let table = Table::new(rows, widths)
            .row_highlight_style(Color::Green)
            .highlight_symbol(">");

        frame.render_stateful_widget(table, table_area, &mut self.ts);

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

        if let Some(prompt) = &self.passphrase_prompt {
            let [area] = Layout::vertical([Constraint::Length(3)])
                .flex(Flex::Center)
                .areas(frame.area());
            let [area] = Layout::horizontal([Constraint::Percentage(60)])
                .flex(Flex::Center)
                .areas(area);
            let block = Block::bordered()
                .title(Line::styled(
                    format!("Passphrase for {}", prompt.block.dev),
                    Color::White,
                ))
                .title_bottom(PROMPT_HELP)
                .title_alignment(Alignment::Center)
                .border_type(BorderType::Rounded)
                .border_style(Color::Yellow);
            let hidden_passphrase = "*".repeat(prompt.passphrase.chars().count());
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(hidden_passphrase).block(block), area);
        }
    }
}

fn short_mount_path(block: &drives::Block) -> String {
    let path = block.mount.as_deref().unwrap_or_default();
    if path.chars().count() <= MAX_MOUNT_TEXT_LENGTH {
        return path.to_owned();
    }
    let start: String = path.chars().take(MAX_MOUNT_TEXT_LENGTH - 1).collect();
    format!("{start}…")
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
    use tokio::sync::oneshot;

    fn tui_with_locked_luks_row() -> Tui {
        let block = drives::Block {
            object_path: "/org/freedesktop/UDisks2/block_devices/sdc2".to_owned(),
            drive_path: "/org/freedesktop/UDisks2/drives/disk".to_owned(),
            dev: "/dev/sdc2".to_owned(),
            label: String::new(),
            mount: None,
            fstype: "crypto_LUKS".to_owned(),
            mounted: false,
            size: String::new(),
            encryption: drives::Encryption::Locked,
        };
        Tui {
            selected: Some(block),
            ..Tui::default()
        }
    }

    fn press(tui: &mut Tui, code: KeyCode) -> InputResult {
        tui.input(KeyEvent::from(code))
    }

    fn type_text(tui: &mut Tui, text: &str) {
        for character in text.chars() {
            press(tui, KeyCode::Char(character));
        }
    }

    // udisks cannot mount a locked device. `m` must ask for the passphrase of
    // the selected device and must not send a mount request.
    #[test]
    fn mount_of_locked_device_asks_for_passphrase() {
        let mut tui = tui_with_locked_luks_row();
        press(&mut tui, KeyCode::Char('m'));
        let prompt = tui.passphrase_prompt.expect("no passphrase prompt");
        assert_eq!(prompt.block.dev, "/dev/sdc2");
        assert_eq!(tui.last_status, "");
    }

    // A passphrase can contain command keys, for example `q`. In the prompt
    // they must become passphrase text, and must not quit or start a command.
    #[test]
    fn prompt_takes_command_keys_as_passphrase_text() {
        let mut tui = tui_with_locked_luks_row();
        press(&mut tui, KeyCode::Char('m'));
        for character in "qmuejkl".chars() {
            let result = press(&mut tui, KeyCode::Char(character));
            assert!(matches!(result, InputResult::None));
        }
        press(&mut tui, KeyCode::Backspace);
        let prompt = tui.passphrase_prompt.expect("prompt closed");
        assert_eq!(prompt.passphrase, "qmuejk");
    }

    // Esc must cancel: no unlock request (it sets a status), and the normal
    // keys work again.
    #[test]
    fn escape_closes_prompt_without_unlock() {
        let mut tui = tui_with_locked_luks_row();
        press(&mut tui, KeyCode::Char('m'));
        type_text(&mut tui, "secret");
        let result = press(&mut tui, KeyCode::Esc);
        assert!(matches!(result, InputResult::None));
        assert!(tui.passphrase_prompt.is_none());
        assert_eq!(tui.last_status, "");
        assert!(matches!(
            press(&mut tui, KeyCode::Char('q')),
            InputResult::Quit
        ));
    }

    fn render_row(dev: &str, mount: &str, terminal_width: u16) -> String {
        let mut tui = Tui {
            drv: vec![drives::Drive {
                id: String::new(),
                object_path: "/drive".to_owned(),
                model: String::new(),
                ejectable: false,
                blocks: vec![drives::Block {
                    object_path: String::new(),
                    drive_path: "/drive".to_owned(),
                    dev: dev.to_owned(),
                    label: "photos".to_owned(),
                    mount: Some(mount.to_owned()),
                    fstype: "exfat".to_owned(),
                    mounted: true,
                    size: "64 GB".to_owned(),
                    encryption: drives::Encryption::Unencrypted,
                }],
            }],
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(terminal_width, 20)).unwrap();
        terminal.draw(|frame| tui.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width.into())
            .map(|line| line.iter().map(ratatui::buffer::Cell::symbol).collect())
            .find(|line: &String| line.starts_with("│ >"))
            .unwrap()
    }

    fn column(row: &str, text: &str) -> usize {
        row.find(text).expect(text)
    }

    // The user identifies a device by its label and fstype.
    // A mount path of up to 30 characters must stay visible in full.
    #[test]
    fn row_shows_fstype_between_label_and_full_mount_path() {
        let row = render_row("/dev/sda1", "/run/media/user/photos", 100);
        assert!(column(&row, "photos") < column(&row, "exfat"));
        assert!(column(&row, "exfat") < column(&row, "/run/media/user/photos"));
    }

    // The free width goes to all columns, in proportion to the length of their content.
    // A short label must not get as much width as a long mount path.
    #[test]
    fn free_width_follows_content_length() {
        let row = render_row("/dev/sda1", "/run/media/user/photos", 100);
        let dev_width = column(&row, "photos") - column(&row, "/dev/sda1");
        let label_width = column(&row, "exfat") - column(&row, "photos");
        let mount_width = column(&row, "64 GB") - column(&row, "/run/media/user/photos");
        assert!(dev_width > "/dev/sda1 ".len());
        assert!(label_width < dev_width);
        assert!(dev_width < mount_width);
    }

    // Network mounts (sshfs, NFS) have long dev names.
    // The other columns must stay visible, mainly the mount path, because `l` goes to it.
    #[test]
    fn long_dev_name_does_not_hide_other_columns() {
        let row = render_row(
            "user@fileserver.example.com:/srv/share",
            "/run/media/user/photos",
            80,
        );
        assert!(column(&row, "photos") < column(&row, "exfat"));
        assert!(column(&row, "exfat") < column(&row, "/run/media/user/photos"));
    }

    // The mount column gets its width from the text in the table, not from the full path.
    // Else a long path still takes the width from the other columns, and they move when it gets mounted.
    #[test]
    fn mount_column_width_follows_short_text() {
        let uuid_path = "/run/media/user/d06c58e9-6860-4a35-a0c0-acd309f8854e";
        let long_path_row = render_row("/dev/sda1", uuid_path, 100);
        let path_of_30_characters_row =
            render_row("/dev/sda1", "/run/media/user/d06c58e9-68601", 100);
        assert_eq!(
            column(&long_path_row, "exfat"),
            column(&path_of_30_characters_row, "exfat")
        );
    }

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
            encryption: drives::Encryption::Unencrypted,
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
    #[test]
    fn selection_stays_on_device_when_rows_move() {
        let mut tui = Tui::default();
        let rows = vec![
            udisks_block("/dev/sdb1", None),
            udisks_block("/dev/sdc1", None),
        ];
        show(&mut tui, rows.clone());
        tui.input(KeyCode::Down.into());
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
    #[test]
    fn down_key_moves_to_next_device() {
        let mut tui = Tui::default();
        let blocks = vec![
            udisks_block("/dev/sda1", None),
            udisks_block("/dev/sdb1", None),
        ];
        show(&mut tui, blocks.clone());
        tui.input(KeyCode::Down.into());
        show(&mut tui, blocks);
        assert_eq!(selected(&tui).dev, "/dev/sdb1");
    }

    // fstab can have many lines with the same dev, for example btrfs subvolumes.
    // These rows have no udisks object path, so only the mount path tells them apart.
    #[test]
    fn fstab_rows_with_same_dev_stay_apart() {
        let mut tui = Tui::default();
        let subvolumes = vec![
            fstab_block("UUID=1234", "/home"),
            fstab_block("UUID=1234", "/var/log"),
        ];
        show(&mut tui, subvolumes.clone());
        tui.input(KeyCode::Down.into());
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
    #[test]
    fn right_key_changes_directory() {
        let mut tui = Tui::default();
        show(&mut tui, vec![udisks_block("/dev/sda1", Some("/mnt/a"))]);
        let result = tui.input(KeyCode::Right.into());
        assert!(matches!(result, InputResult::QuitChangeDirectory(path) if path == "/mnt/a"));
    }

    // udisks names the mount directory of a filesystem without a label by its UUID, so the path is long.
    // The table shows only the start of a long path, but `l` must still go to the full path.
    #[test]
    fn long_mount_path_is_short_in_table_but_full_for_cd() {
        let mut tui = Tui::default();
        let uuid_path = "/run/media/user/d06c58e9-6860-4a35-a0c0-acd309f8854e";
        show(&mut tui, vec![udisks_block("/dev/sdc1", Some(uuid_path))]);
        assert!(draw(&mut tui).contains("/run/media/user/d06c58e9-6860…"));
        let result = tui.input(KeyCode::Right.into());
        assert!(matches!(result, InputResult::QuitChangeDirectory(path) if path == uuid_path));
    }

    // A label can have characters of more than one byte. A cut inside a character stops the program.
    #[test]
    fn long_mount_path_is_cut_between_characters() {
        let block = udisks_block("/dev/sdc1", Some("/run/media/user/Фотографии отпуск 2024"));
        assert_eq!(short_mount_path(&block), "/run/media/user/Фотографии от…");
    }

    // An unmount of a slow USB stick must first write its cache, and a LUKS unlock must derive the key.
    // During that time the user must see that the operation runs, and the keys must still work.
    #[tokio::test]
    async fn screen_and_keys_work_while_operation_runs() {
        let mut tui = Tui::default();
        let rows = vec![
            udisks_block("/dev/sda1", None),
            udisks_block("/dev/sdb1", None),
        ];
        show(&mut tui, rows.clone());
        let (end_operation, operation_ended) = oneshot::channel();
        tui.start("Unmounting...", async { operation_ended.await.unwrap() });

        tui.input(KeyCode::Down.into());
        show(&mut tui, rows);
        assert_eq!(selected(&tui).dev, "/dev/sdb1");
        assert!(draw(&mut tui).contains("Unmounting..."));

        end_operation.send(Err(udisks2::Error::Failed)).unwrap();
        tokio::task::yield_now().await;
        tui.finish_operation().await;
        assert!(draw(&mut tui).contains("Error: The operation failed"));
    }

    // A second operation would replace the result of the first in the status line, and the user could miss an error.
    // A udisks call can hang (a polkit password request that the terminal does not show), so `q` must still work.
    #[tokio::test]
    async fn mount_keys_do_nothing_but_quit_works_while_operation_runs() {
        let mut tui = Tui::default();
        let fake_device = "/org/freedesktop/UDisks2/block_devices/mmtui_test";
        show(&mut tui, vec![block(fake_device, "/dev/mmtui_test", None)]);
        let (_end_operation, operation_ended) = oneshot::channel();
        tui.start("Unmounting...", async { operation_ended.await.unwrap() });

        for key in ['m', 'u', 'e'] {
            tui.input(KeyCode::Char(key).into());
            assert_eq!(tui.last_status, "Unmounting...");
        }
        assert!(matches!(
            tui.input(KeyCode::Char('q').into()),
            InputResult::Quit
        ));
    }

    // The status line must show the text of an error, not its debug structure.
    #[test]
    fn status_shows_text_of_error() {
        let mut tui = Tui::default();
        let error = udisks2::Error::Zbus(udisks2::zbus::Error::InterfaceNotFound);
        tui.set_status(Err(error));
        assert_eq!(tui.last_status, "Error: Interface not found");
    }
}
