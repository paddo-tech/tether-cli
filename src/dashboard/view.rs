use super::app::{App, Overlay, Tab};
use super::components::{
    backdrop, config, confirm, file_import, files, header, help, machines, overview, packages,
    palette, pkg_import, profile_picker, tabs, toast,
};
use ratatui::{prelude::*, widgets::Block};

pub fn view(f: &mut Frame, app: &App) {
    app.hits.borrow_mut().clear();
    let t = &app.theme;
    f.render_widget(
        Block::default().style(Style::default().bg(t.base_bg).fg(t.text)),
        f.area(),
    );

    let [head, tab_bar, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area().inner(Margin {
        horizontal: 1,
        vertical: 0,
    }));

    header::render(f, head, app);
    tabs::render(f, tab_bar, app);

    match app.active_tab {
        Tab::Overview => overview::render(f, body, app),
        Tab::Files => files::render(f, body, app),
        Tab::Packages => packages::render(f, body, app),
        Tab::Machines => machines::render(f, body, app),
        Tab::Config => config::render(f, body, app),
    }

    help::render_bar(f, footer, app);

    for overlay in &app.overlays {
        if overlay.is_modal() {
            backdrop(f, t);
            // Only the modal's own regions stay clickable.
            app.hits.borrow_mut().clear();
        }
        match overlay {
            Overlay::Help => help::render_overlay(f, t),
            Overlay::Confirm(c) => confirm::render(f, app, c),
            Overlay::FileImport(p) => file_import::render(f, app, p),
            Overlay::PkgImport(p) => pkg_import::render(f, app, p),
            Overlay::ProfilePicker(p) => profile_picker::render(f, app, p),
            Overlay::Palette(p) => palette::render(f, app, p),
        }
    }

    toast::render(f, &app.toasts, t);
}
