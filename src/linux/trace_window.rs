//! The Text Trace window: choose Dictations and parts, see exactly what will be exported, and
//! export or clear. Nothing is written to disk except by "Export selected".

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::{gio, glib, prelude::*};

use crate::trace::{ExportOptions, TraceStore};

pub type SharedStore = Rc<RefCell<TraceStore>>;

/// Opens the window, or brings it to the front. `store` is `None` while the trace is off.
pub fn open(application: &gtk4::Application, store: Option<SharedStore>) {
    if let Some(window) = application
        .windows()
        .into_iter()
        .find(|window| window.widget_name() == "livetyper-text-trace")
    {
        window.present();
        return;
    }
    let window = gtk4::ApplicationWindow::builder()
        .application(application)
        .title("LiveTyper Text Trace")
        .default_width(760)
        .default_height(640)
        .build();
    window.set_widget_name("livetyper-text-trace");
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    window.set_child(Some(&content));

    let Some(store) = store else {
        let label = gtk4::Label::new(Some(
            "The Text Trace is off, so nothing is recorded.\n\n\
             To turn it on, set text_trace = true in ~/.config/livetyper/settings.toml \
             and restart LiveTyper. Read the Text Trace section of the README first.",
        ));
        label.set_wrap(true);
        label.set_xalign(0.0);
        content.append(&label);
        window.present();
        return;
    };

    let intro = gtk4::Label::new(Some(
        "These Dictations are kept in memory only. Quitting LiveTyper or Clear all deletes them. \
         Tick the Dictations and parts to export, check the preview, then export. \
         The export is the only thing written to disk.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    content.append(&intro);

    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let list_scroll = gtk4::ScrolledWindow::builder()
        .child(&list)
        .min_content_height(120)
        .max_content_height(200)
        .propagate_natural_height(true)
        .build();
    content.append(&list_scroll);

    let options_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let defaults = ExportOptions::default();
    let paste = gtk4::CheckButton::with_label("Paste details and read-back");
    paste.set_active(defaults.paste);
    let steps = gtk4::CheckButton::with_label("Recast steps");
    steps.set_active(defaults.steps);
    let real_words = gtk4::CheckButton::with_label("Real words");
    real_words.set_active(defaults.real_words);
    let audio = gtk4::CheckButton::with_label("Audio");
    audio.set_active(defaults.audio);
    for button in [&paste, &steps, &real_words, &audio] {
        options_row.append(button);
    }
    content.append(&options_row);

    let preview = gtk4::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .build();
    let preview_scroll = gtk4::ScrolledWindow::builder()
        .child(&preview)
        .vexpand(true)
        .build();
    content.append(&gtk4::Label::new(Some("Preview of the export:")));
    content.append(&preview_scroll);

    let status = gtk4::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_selectable(true);
    content.append(&status);

    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let refresh = gtk4::Button::with_label("Refresh");
    let clear = gtk4::Button::with_label("Clear all");
    let export = gtk4::Button::with_label("Export selected…");
    buttons.append(&refresh);
    buttons.append(&clear);
    buttons.append(&export);
    content.append(&buttons);

    let selected: Rc<RefCell<HashSet<u64>>> = Rc::default();
    let ui = Rc::new(Ui {
        store,
        list,
        preview,
        status,
        paste,
        steps,
        real_words,
        audio,
        selected,
    });

    for button in [&ui.paste, &ui.steps, &ui.real_words, &ui.audio] {
        let ui = ui.clone();
        button.connect_toggled(move |_| ui.update_preview());
    }
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| ui.fill_list());
    }
    {
        let ui = ui.clone();
        clear.connect_clicked(move |_| {
            ui.store.borrow_mut().clear();
            ui.selected.borrow_mut().clear();
            ui.fill_list();
            ui.status
                .set_text("All recorded Dictations were deleted from memory.");
        });
    }
    {
        let ui = ui.clone();
        export.connect_clicked(move |_| ui.export());
    }
    ui.fill_list();
    window.present();
}

struct Ui {
    store: SharedStore,
    list: gtk4::Box,
    preview: gtk4::TextView,
    status: gtk4::Label,
    paste: gtk4::CheckButton,
    steps: gtk4::CheckButton,
    real_words: gtk4::CheckButton,
    audio: gtk4::CheckButton,
    selected: Rc<RefCell<HashSet<u64>>>,
}

impl Ui {
    fn options(&self) -> ExportOptions {
        ExportOptions {
            paste: self.paste.is_active(),
            steps: self.steps.is_active(),
            real_words: self.real_words.is_active(),
            audio: self.audio.is_active(),
        }
    }

    fn ids(&self) -> Vec<u64> {
        let selected = self.selected.borrow();
        self.store
            .borrow()
            .items()
            .map(|item| item.id)
            .filter(|id| selected.contains(id))
            .collect()
    }

    fn fill_list(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let store = self.store.borrow();
        // Forget selections of Dictations that are no longer kept.
        self.selected
            .borrow_mut()
            .retain(|id| store.items().any(|item| item.id == *id));
        if store.is_empty() {
            self.list.append(&gtk4::Label::new(Some(
                "No Dictations recorded yet. Dictate, then press Refresh.",
            )));
        }
        for item in store.items().collect::<Vec<_>>().into_iter().rev() {
            let words = item
                .engine
                .as_ref()
                .map(|engine| first_words(&engine.final_text))
                .unwrap_or_default();
            let button = gtk4::CheckButton::with_label(&format!(
                "Dictation {} · {} · {}",
                item.id, item.time, words
            ));
            button.set_active(self.selected.borrow().contains(&item.id));
            let id = item.id;
            let ui = self.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    ui.selected.borrow_mut().insert(id);
                } else {
                    ui.selected.borrow_mut().remove(&id);
                }
                ui.update_preview();
            });
            self.list.append(&button);
        }
        drop(store);
        self.update_preview();
    }

    fn update_preview(&self) {
        let ids = self.ids();
        let text = if ids.is_empty() {
            "Tick at least one Dictation to see what would be exported.".to_owned()
        } else {
            self.store.borrow().render(&ids, self.options())
        };
        self.preview.buffer().set_text(&text);
    }

    fn export(&self) {
        let ids = self.ids();
        if ids.is_empty() {
            self.status
                .set_text("Tick at least one Dictation to export.");
            return;
        }
        let base = glib::user_special_dir(glib::UserDirectory::Downloads)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(glib::home_dir);
        let stamp = glib::DateTime::now_local()
            .and_then(|now| now.format("%Y%m%d-%H%M%S"))
            .map(|stamp| stamp.to_string())
            .unwrap_or_else(|_| "export".into());
        let folder = base.join(format!("livetyper-text-trace-{stamp}"));
        match write_export(&self.store.borrow(), &ids, self.options(), &folder) {
            Ok(folder) => {
                log::info!("Text Trace: {} Dictations exported", ids.len());
                self.status.set_text(&format!(
                    "Exported to {}\nCheck the files before you send them. Delete the folder when you no longer need it.",
                    folder.display()
                ));
                let uri = gio::File::for_path(&folder).uri();
                let _ = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
            }
            Err(error) => {
                log::warn!("Text Trace export failed: {error}");
                self.status.set_text(&format!("Export failed: {error}"));
            }
        }
    }
}

fn first_words(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(5).collect();
    let more = text.split_whitespace().count() > words.len();
    format!("{}{}", words.join(" "), if more { "…" } else { "" })
}

/// Writes the export into `folder`, a new folder that only the user can open.
fn write_export(
    store: &TraceStore,
    ids: &[u64],
    options: ExportOptions,
    folder: &Path,
) -> std::io::Result<PathBuf> {
    fs::DirBuilder::new().mode(0o700).create(folder)?;
    write_private(
        &folder.join("text-trace.txt"),
        store.render(ids, options).as_bytes(),
    )?;
    if options.audio {
        for &id in ids {
            if let Some(wav) = store.wav(id) {
                write_private(&folder.join(format!("dictation-{id}.wav")), &wav)?;
            }
        }
    }
    Ok(folder.to_owned())
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use crate::trace::EngineTrace;

    #[test]
    fn export_writes_only_the_chosen_parts_into_a_private_folder() {
        let mut store = TraceStore::new(5);
        let chosen = store.push(
            "10:00:00".into(),
            Some(EngineTrace {
                final_text: "Hello there".into(),
                audio: Some(vec![0; 16]),
                ..EngineTrace::default()
            }),
        );
        let other = store.push(
            "10:01:00".into(),
            Some(EngineTrace {
                final_text: "Other secret".into(),
                audio: Some(vec![0; 16]),
                ..EngineTrace::default()
            }),
        );
        let folder =
            std::env::temp_dir().join(format!("livetyper-export-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);

        write_export(&store, &[chosen], ExportOptions::default(), &folder).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        let text_file = folder.join("text-trace.txt");
        assert_eq!(mode(&text_file), 0o600);
        let text = fs::read_to_string(&text_file).unwrap();
        assert!(text.contains("Aaaaa·aaaaa"));
        assert!(!text.contains("Hello") && !text.contains("Other") && !text.contains("secret"));
        assert!(!text.contains(&format!("Dictation {other} ")));
        // Audio stays out unless it is ticked.
        assert!(!folder.join(format!("dictation-{chosen}.wav")).exists());
        // An export never overwrites an existing folder.
        assert!(write_export(&store, &[chosen], ExportOptions::default(), &folder).is_err());
        fs::remove_dir_all(&folder).unwrap();

        let with_audio = ExportOptions {
            audio: true,
            ..ExportOptions::default()
        };
        write_export(&store, &[chosen], with_audio, &folder).unwrap();
        let wav = folder.join(format!("dictation-{chosen}.wav"));
        assert_eq!(mode(&wav), 0o600);
        assert!(!folder.join(format!("dictation-{other}.wav")).exists());
        fs::remove_dir_all(&folder).unwrap();
    }
}
