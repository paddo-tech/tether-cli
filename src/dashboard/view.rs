use super::app::{App, Overlay, Tab};
use super::components::{
    config, confirm, file_import, files, header, help, machines, overview, packages, pkg_import,
    profile_picker, tabs,
};
use ratatui::prelude::*;

pub fn view(f: &mut Frame, app: &App) {
    let main_chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(f.area());
    let content_chunks =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(main_chunks[1]);

    header::render(f, main_chunks[0], app);
    tabs::render(f, content_chunks[0], app);

    let body = content_chunks[1];
    match app.active_tab {
        Tab::Overview => overview::render(f, body, app),
        Tab::Files => files::render(f, body, app),
        Tab::Packages => packages::render(f, body, app),
        Tab::Machines => machines::render(f, body, app),
        Tab::Config => config::render(f, body, app),
    }

    help::render_bar(f, main_chunks[2], app.active_tab, &app.theme);

    for overlay in &app.overlays {
        match overlay {
            Overlay::Help => help::render_overlay(f, &app.theme),
            Overlay::Confirm(c) => confirm::render(f, c, &app.theme),
            Overlay::FileImport(p) => file_import::render(f, p, &app.theme),
            Overlay::PkgImport(p) => pkg_import::render(f, p, &app.theme),
            Overlay::ProfilePicker(p) => profile_picker::render(f, p, &app.theme),
        }
    }
}
