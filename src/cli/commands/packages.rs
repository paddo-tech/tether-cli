use anyhow::Result;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::inbox::{self, InboxItem, Kind};
use crate::packages::{BrewManager, PackageInfo, PackageManager};
use crate::sync::membership::{self, Edit, Membership};

/// Installed packages under one manager key, such as `brew_casks` or `npm`.
struct Installed {
    key: &'static str,
    packages: Vec<PackageInfo>,
}

/// Installed packages under the manager keys that package ids use. A manager that fails
/// to list is reported and skipped.
async fn installed() -> Vec<Installed> {
    let mut lists: Vec<(&'static str, Result<Vec<PackageInfo>>)> = Vec::new();
    let brew = BrewManager::new();
    if brew.is_available().await {
        lists.push(("brew_formulae", brew.list_installed().await));
        let casks = brew.list_installed_casks().await.map(|names| {
            names
                .into_iter()
                .map(|name| PackageInfo {
                    name,
                    version: None,
                })
                .collect()
        });
        lists.push(("brew_casks", casks));
    }
    for key in ["npm", "pnpm", "bun", "gem", "uv"] {
        let manager = crate::packages::manager_for_key(key).expect("a manager key");
        if manager.is_available().await {
            lists.push((key, manager.list_installed().await));
        }
    }
    lists
        .into_iter()
        .filter_map(|(key, list)| match list {
            Ok(packages) if !packages.is_empty() => Some(Installed { key, packages }),
            Ok(_) => None,
            Err(e) => {
                Output::warning(&format!("Failed to list {} packages: {}", key, e));
                None
            }
        })
        .collect()
}

/// List installed packages by manager key, with the profiles each belongs to.
pub async fn list(json: bool) -> Result<()> {
    let installed = installed().await;
    let membership = crate::config::Config::load()
        .and_then(|config| Membership::load_current(&config))
        .ok();
    if json {
        return Output::json(&list_json(&installed, membership.as_ref()));
    }
    print_install_failures();
    if installed.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }
    if let Some(m) = &membership {
        Output::info(&format!(
            "This machine installs the packages of profile {}",
            m.profile
        ));
    }
    for group in &installed {
        Output::section(group.key);
        for pkg in &group.packages {
            let mut display = match &pkg.version {
                Some(v) => format!("{} {}", pkg.name, v),
                None => pkg.name.clone(),
            };
            if let Some(m) = &membership {
                display.push_str(&format!("  ({})", members_label(m, group.key, &pkg.name)));
            }
            Output::list_item(&display);
        }
    }
    println!();
    Output::dim(
        "A package id is manager:name, such as npm:typescript. Run 'tether packages share <id> --to <profile>' to add a profile",
    );
    Ok(())
}

/// `packages list --json`: `profile` is this machine's profile, or null without a sync repo.
/// Each package has `id`, `manager`, `name`, `version` and `profiles`.
fn list_json(installed: &[Installed], membership: Option<&Membership>) -> serde_json::Value {
    let packages: Vec<serde_json::Value> = installed
        .iter()
        .flat_map(|g| {
            g.packages.iter().map(move |p| {
                let profiles: Vec<String> = membership.map_or_else(Vec::new, |m| {
                    let members = m.members(g.key, &p.name);
                    if members.is_empty() {
                        vec![m.profile.clone()]
                    } else {
                        members.into_iter().collect()
                    }
                });
                serde_json::json!({
                    "id": format!("{}:{}", g.key, p.name),
                    "manager": g.key,
                    "name": p.name,
                    "version": p.version,
                    "profiles": profiles,
                })
            })
        })
        .collect();
    let failures: Vec<serde_json::Value> = crate::sync::SyncState::load()
        .map(|s| {
            let mut failures: Vec<_> = s.install_failures.into_iter().collect();
            failures.sort_by(|a, b| a.0.cmp(&b.0));
            failures
                .into_iter()
                .map(|(id, f)| {
                    serde_json::json!({
                        "id": id,
                        "version": f.version,
                        "attempted": f.attempted,
                        "error": f.error,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "profile": membership.map(|m| m.profile.clone()),
        "packages": packages,
        "install_failures": failures,
    })
}

/// Pick installed packages to uninstall, in a terminal. Each one is uninstalled as by
/// `tether packages uninstall <id>`.
pub async fn pick_uninstall() -> Result<()> {
    if !Prompt::is_interactive() {
        anyhow::bail!("Name the package to uninstall: tether packages uninstall manager:name");
    }
    let ids: Vec<String> = installed()
        .await
        .iter()
        .flat_map(|g| g.packages.iter().map(|p| format!("{}:{}", g.key, p.name)))
        .collect();
    if ids.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }
    let selected = Prompt::multi_select(
        "Select packages to uninstall",
        ids.iter().map(String::as_str).collect(),
        &[],
    )?;
    if selected.is_empty() {
        Output::info("No packages selected");
        return Ok(());
    }
    let chosen: Vec<&str> = selected.iter().map(|&i| ids[i].as_str()).collect();
    if !Prompt::confirm(&format!("Uninstall {}?", chosen.join(", ")), false)? {
        return Ok(());
    }
    let mut failed = 0;
    for id in chosen {
        if let Err(e) = remove(id).await {
            Output::warning(&format!("{}: {:#}", id, e));
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!("{} package(s) were not uninstalled", failed);
    }
    Ok(())
}

/// Synced packages that failed to install here. Syncs retry them after a day, or when the
/// version changes.
fn print_install_failures() {
    let Ok(state) = crate::sync::SyncState::load() else {
        return;
    };
    if state.install_failures.is_empty() {
        return;
    }
    Output::section("Failed to install on this machine");
    let mut failures: Vec<_> = state.install_failures.iter().collect();
    failures.sort_by_key(|(key, _)| *key);
    for (key, failure) in failures {
        let version = failure
            .version
            .as_deref()
            .map(|v| format!(" {}", v))
            .unwrap_or_default();
        Output::list_item(&format!(
            "{}{} ({}): {}",
            key,
            version,
            crate::cli::output::relative_time(failure.attempted),
            failure.error.lines().next().unwrap_or_default()
        ));
    }
    println!();
}

/// The package's member profiles, or "this profile only". A package no record lists yet
/// joins this machine's profile on the next sync.
pub fn members_label(membership: &Membership, manager: &str, name: &str) -> String {
    let members = membership.members(manager, name);
    if members.is_empty() || (members.len() == 1 && members.contains(&membership.profile)) {
        "this profile only".to_string()
    } else {
        members.into_iter().collect::<Vec<_>>().join(", ")
    }
}

/// Manager key and name of a `manager:name` id. `brew:` and `cask:` stand for
/// `brew_formulae:` and `brew_casks:`.
fn split_id(id: &str) -> Result<(&'static str, &str)> {
    match id.split_once(':') {
        Some((manager, name)) if !name.is_empty() => {
            if let Some(key) = crate::packages::key_of_manager(manager) {
                return Ok((key, name));
            }
        }
        _ => {}
    }
    anyhow::bail!(
        "Name the package as manager:name, such as npm:typescript or cask:zoom. Managers: {}, \
         and brew and cask for brew_formulae and brew_casks",
        crate::packages::MANAGER_KEYS.join(", ")
    )
}

pub fn share(id: &str, to: &[String]) -> Result<()> {
    let (manager, name) = split_id(id)?;
    let config = crate::config::Config::load()?;
    let choices = membership::profile_choices(
        &config,
        &Membership::load_current(&config)?.members(manager, name),
    );
    if let Some(unknown) = to.iter().find(|p| !choices.contains(p.as_str())) {
        anyhow::bail!(
            "No profile '{}'. Profiles: {}",
            unknown,
            choices.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    let edit = Edit {
        add: to.iter().cloned().collect(),
        ..Edit::default()
    };
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let members = membership::save_edit(&config, manager, name, &edit)?.unwrap_or_default();
    Output::success(&format!(
        "{} now belongs to: {}. Machines in these profiles install it on their next sync",
        membership::canonical_id(manager, name),
        members.into_iter().collect::<Vec<_>>().join(", ")
    ));
    Ok(())
}

pub fn unshare(id: &str, from: &[String]) -> Result<()> {
    let (manager, name) = split_id(id)?;
    let config = crate::config::Config::load()?;
    let members = Membership::load_current(&config)?.members(manager, name);
    if let Some(other) = from.iter().find(|p| !members.contains(p.as_str())) {
        anyhow::bail!(
            "{} does not belong to profile {}. Its profiles: {}",
            id,
            other,
            members.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    let edit = Edit {
        remove: from.iter().cloned().collect(),
        ..Edit::default()
    };
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let Some(members) = membership::save_edit(&config, manager, name, &edit)? else {
        anyhow::bail!(
            "A package keeps at least one profile. To drop {} everywhere, run 'tether packages \
             uninstall {}' on a machine of each profile",
            id,
            id
        );
    };
    Output::success(&format!(
        "{} now belongs to: {}. Machines in {} stop installing it, but keep any copy they have",
        membership::canonical_id(manager, name),
        members.into_iter().collect::<Vec<_>>().join(", "),
        from.join(", ")
    ));
    Ok(())
}

/// Install a package another machine lists, as the dashboard's Import does: OSV checks the
/// release that would install, and nothing is checked against trusted records. A package
/// in the inbox needs approval instead, so its review is not skipped.
pub async fn install(id: &str) -> Result<()> {
    let (manager, name) = split_id(id)?;
    let canonical = membership::canonical_id(manager, name);
    if inbox::list()?.iter().any(|i| {
        i.kind == Kind::Package && membership::canonical_id(&i.manager, &i.name) == canonical
    }) {
        anyhow::bail!(
            "{} waits in the inbox. Review it with 'tether packages inbox', then run \
             'tether packages approve {}'",
            id,
            canonical
        );
    }
    let sync_path = crate::sync::SyncEngine::sync_path()?;
    let machine_id = crate::sync::SyncState::load()?.machine_id;
    let lists = |m: &crate::sync::MachineState| {
        m.packages.get(manager).is_some_and(|names| {
            names
                .iter()
                .any(|n| membership::canonical_id(manager, n) == canonical)
        })
    };
    let records = crate::sync::MachineState::list_all(&sync_path)?;
    if records
        .iter()
        .any(|m| m.machine_id == machine_id && lists(m))
    {
        anyhow::bail!("{} is already installed here", id);
    }
    let sources: Vec<&str> = records
        .iter()
        .filter(|m| m.machine_id != machine_id && lists(m))
        .map(|m| m.machine_id.as_str())
        .collect();
    if sources.is_empty() {
        anyhow::bail!("No other machine lists {}", id);
    }
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let Some(version) = osv_checked(manager, name, None).await? else {
        return Ok(());
    };
    Output::info(&format!(
        "Installing {} from {}",
        canonical,
        sources.join(", ")
    ));
    inbox::install(
        &InboxItem {
            kind: Kind::Package,
            manager: manager.to_string(),
            name: name.to_string(),
            version,
            tap: None,
            source_machine: None,
            commit: None,
            signer: None,
            reasons: Vec::new(),
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        },
        true,
    )
    .await?;
    forget_removal(&sync_path, &machine_id, manager, name)?;
    Output::success(&format!("Installed {}", canonical));
    Ok(())
}

/// A sync uninstalls a package this machine removed, so an install takes it off that list.
fn forget_removal(
    sync_path: &std::path::Path,
    machine_id: &str,
    manager: &str,
    name: &str,
) -> Result<()> {
    let Some(mut record) = crate::sync::signing::own_record(sync_path, machine_id)? else {
        return Ok(());
    };
    let Some(removed) = record.removed_packages.get_mut(manager) else {
        return Ok(());
    };
    let before = removed.len();
    removed.retain(|n| {
        membership::canonical_id(manager, n) != membership::canonical_id(manager, name)
    });
    if removed.len() == before {
        return Ok(());
    }
    if removed.is_empty() {
        record.removed_packages.remove(manager);
    }
    crate::sync::signing::save_record(sync_path, &record)
}

pub async fn remove(id: &str) -> Result<()> {
    let (manager, name) = split_id(id)?;
    if manager == "brew_taps" {
        anyhow::bail!(
            "Tether does not remove taps. Run 'brew untap {}' on each machine that has it",
            name
        );
    }
    let config = crate::config::Config::load()?;
    let membership = Membership::load_current(&config)?;
    if let Some(manager) = crate::packages::manager_for_key(manager) {
        let dependents = manager.get_dependents(name).await.unwrap_or_default();
        if !dependents.is_empty() {
            Output::warning(&format!(
                "{} is required by: {}",
                name,
                dependents.join(", ")
            ));
            if !Prompt::confirm(&format!("Uninstall {} anyway?", name), false)? {
                return Ok(());
            }
        }
    }
    // The profile leaves the package only once it is gone here
    crate::packages::uninstall(manager, name).await?;
    Output::success(&format!("Uninstalled {}", id));
    let members = membership.members(manager, name);
    if !members.contains(&membership.profile) || members.len() == 1 {
        return Ok(());
    }
    let edit = Edit {
        remove: [membership.profile.clone()].into(),
        ..Edit::default()
    };
    let saved = crate::sync::acquire_sync_lock(true)
        .and_then(|_lock| membership::save_edit(&config, manager, name, &edit));
    match saved {
        Ok(Some(keep)) => Output::info(&format!(
            "Other machines in profile {} no longer install {}, but keep any copy they have. \
             Profiles {} keep it",
            membership.profile,
            id,
            keep.into_iter().collect::<Vec<_>>().join(", ")
        )),
        Ok(None) => {}
        Err(e) => anyhow::bail!(
            "Uninstalled {}, but saving its profiles failed: {}. Other machines in profile {} \
             still install it",
            id,
            e,
            membership.profile
        ),
    }
    Ok(())
}

fn describe(item: &InboxItem) -> String {
    let mut text = item.id();
    if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
        text.push_str(&format!(" key {}", fingerprint));
    }
    if let Some(version) = &item.version {
        text.push_str(&format!(" {}", version));
    }
    if let Some(tap) = &item.tap {
        text.push_str(&format!(" tap {}", tap));
    }
    if let Some(machine) = &item.source_machine {
        text.push_str(&format!(" from {}", machine));
    }
    let reasons: Vec<&str> = item.reasons.iter().map(|r| r.label()).collect();
    text.push_str(&format!(" ({})", reasons.join(", ")));
    if !item.advisories.is_empty() {
        text.push_str(&format!(" [OSV: {}]", item.advisories.join(", ")));
    }
    text
}

/// List packages waiting for approval.
pub async fn inbox_list(json: bool) -> Result<()> {
    let items = inbox::list()?;
    if json {
        return Output::json(&serde_json::Value::Array(
            items.iter().map(inbox_item_json).collect(),
        ));
    }
    if items.is_empty() {
        Output::info("No packages wait for approval");
        return Ok(());
    }
    Output::section("Packages waiting for approval");
    let mut items = items;
    inbox::sort_by_group(&mut items);
    for group in inbox::groups(&items) {
        Output::subheader(&group_line(&group));
        for &i in &group.items {
            Output::list_item(&describe(&items[i]));
        }
    }
    Output::dim(
        "Run 'tether packages approve <id> --expect <version, tap or key>', 'tether packages approve --all', or 'tether packages reject <id> --expect <version, tap or key>'",
    );
    Ok(())
}

/// One `packages inbox --json` item. `expect` is what `approve --expect` takes, and
/// `bulk_approvable` whether `approve --all` covers it.
fn inbox_item_json(item: &InboxItem) -> serde_json::Value {
    let fingerprint = match &item.kind {
        Kind::TrustMachine { fingerprint, .. } => Some(fingerprint.as_str()),
        Kind::Package => None,
    };
    serde_json::json!({
        "id": item.id(),
        "kind": if fingerprint.is_some() { "machine_key" } else { "package" },
        "manager": item.manager,
        "name": item.name,
        "version": item.version,
        "tap": item.tap,
        "fingerprint": fingerprint,
        "from": item.from_machine(),
        "reasons": item.reasons,
        "advisories": item.advisories,
        "expect": binding(item),
        "bulk_approvable": item.bulk_approvable(),
        "first_seen": item.first_seen,
    })
}

/// What a sync can replace under an item's id and the user must confirm: the key
/// fingerprint, the version, or the Homebrew tap.
fn binding(item: &InboxItem) -> Option<&str> {
    match &item.kind {
        Kind::TrustMachine { fingerprint, .. } => Some(fingerprint),
        Kind::Package => item.version.as_deref().or(item.tap.as_deref()),
    }
}

fn check_expected(item: &InboxItem, expected: &str) -> Result<()> {
    if binding(item) != Some(expected) {
        anyhow::bail!(
            "{} is now {}, not {}. Review it with 'tether packages inbox'",
            item.id(),
            binding(item).unwrap_or("unpinned"),
            expected
        );
    }
    Ok(())
}

/// The item `id` as the user reviewed it: shown and confirmed in a terminal, or checked
/// against the version, tap or key they named. So a replacement that a sync queued under
/// the same id is not decided on. None when the user declines.
fn reviewed(
    id: &str,
    expected: Option<&str>,
    action: &str,
    question: &str,
) -> Result<Option<InboxItem>> {
    let item = inbox::Inbox::load()?.find(id)?.clone();
    match expected {
        Some(expected) => check_expected(&item, expected)?,
        None if Prompt::is_interactive() && !Prompt::assume_yes() => {
            Output::info(&describe(&item));
            if !Prompt::confirm(question, false)? {
                return Ok(None);
            }
        }
        None => {
            if let Some(binding) = binding(&item) {
                anyhow::bail!(
                    "Check {}, then run 'tether packages {} {} --expect {}'",
                    describe(&item),
                    action,
                    item.id(),
                    binding
                );
            }
        }
    }
    Ok(Some(item))
}

/// Approve a held package and install it now, or trust a held machine key. The sync lock
/// keeps the daemon from installing the same package while this install runs.
pub async fn approve(id: &str, expected: Option<&str>) -> Result<()> {
    if let Some(machine) = id.strip_prefix("machine:") {
        return super::machines::trust(machine, expected).await;
    }
    let Some(item) = reviewed(id, expected, "approve", "Approve this item?")? else {
        return Ok(());
    };
    // Naming the version on the command line already confirms it
    if expected.is_none() && item.signature_failed() && !confirm_signature_failed(&item)? {
        return Ok(());
    }
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    Output::info(&format!("Approving {}", describe(&item)));
    approve_locked(&item).await
}

/// A package whose source machine's record fails its signature may be a forged manifest
/// line, so the user types its version or tap and answers a second question.
fn confirm_signature_failed(item: &InboxItem) -> Result<bool> {
    Output::warning(&format!(
        "{} comes from {}, whose record fails its signature. Someone may have edited it in \
         the repo. Do not approve it unless you know why the signature fails",
        item.name,
        item.source_machine.as_deref().unwrap_or("another machine")
    ));
    if let Some(binding) = binding(item) {
        if Prompt::input(&format!("Type {} to approve it", binding), None)?.trim() != binding {
            Output::info(&format!("Not approved: {}", item.id()));
            return Ok(false);
        }
    }
    Prompt::question("Approve it anyway?", false)
}

/// Approve exactly the item shown to the user. The caller holds the sync lock.
async fn approve_locked(shown: &InboxItem) -> Result<()> {
    let mut version = None;
    if shown.kind == Kind::Package {
        match osv_checked(&shown.manager, &shown.name, shown.version.as_deref()).await? {
            Some(checked) => version = checked,
            None => return Ok(()),
        }
    }
    let item = inbox::approve(shown)?;
    if let Kind::TrustMachine { fingerprint, .. } = &item.kind {
        Output::success(&format!(
            "Trusted machine {} with key {}",
            item.name, fingerprint
        ));
        return Ok(());
    }
    Output::info(&format!("Approved {}. Installing...", item.id()));
    inbox::install(
        &InboxItem {
            version,
            ..item.clone()
        },
        true,
    )
    .await?;
    Output::success(&format!("Installed {}", item.name));
    Ok(())
}

/// Check OSV before the package is approved, and return the version to install. When OSV
/// could not check the release that would install, a terminal user may install it anyway.
/// Without a terminal the approval fails, so nothing installs unchecked. None when the user
/// declines.
async fn osv_checked(
    manager: &str,
    name: &str,
    version: Option<&str>,
) -> Result<Option<Option<String>>> {
    let e = match inbox::check_osv(manager, name, version, true).await {
        Ok(version) => return Ok(Some(version)),
        Err(e) => e,
    };
    let unchecked = match e.downcast::<inbox::OsvUnchecked>() {
        Ok(unchecked) if Prompt::is_interactive() && !Prompt::assume_yes() => unchecked,
        Ok(unchecked) => return Err(unchecked.into()),
        Err(e) => return Err(e),
    };
    Output::warning(&unchecked.to_string());
    if Prompt::question("Install it without the malicious-package check?", false)? {
        Ok(Some(unchecked.version))
    } else {
        Ok(None)
    }
}

/// Approve and install every package one answer may cover, as listed when asked. A sync
/// that queues more meanwhile does not add to them. With `from`, only items from that
/// machine.
pub async fn approve_all(from: Option<&str>) -> Result<()> {
    let from = match from {
        Some(m) => Some(super::machines::resolve(
            &crate::sync::SyncEngine::sync_path()?,
            m,
        )?),
        None => None,
    };
    let mut items = inbox::list()?;
    inbox::sort_by_group(&mut items);
    let (approvable, held): (Vec<InboxItem>, Vec<InboxItem>) = items
        .into_iter()
        .filter(|i| from.as_deref().is_none_or(|m| i.from_machine() == Some(m)))
        .partition(InboxItem::bulk_approvable);
    if !held.is_empty() {
        Output::info(&format!(
            "{} item(s) need their own decision and stay in the inbox: machine keys, packages \
             OSV lists as malicious, and packages whose record fails its signature",
            held.len()
        ));
    }
    if approvable.is_empty() {
        Output::info("No packages to approve");
        return Ok(());
    }
    Output::section("Packages to approve and install");
    for item in &approvable {
        Output::list_item(&describe(item));
    }
    if !Prompt::confirm(
        &format!("Approve and install these {} packages?", approvable.len()),
        false,
    )? {
        return Ok(());
    }
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    let mut failed = 0;
    for item in &approvable {
        if let Err(e) = approve_locked(item).await {
            Output::warning(&format!("{}: {:#}", item.id(), e));
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!(
            "{} of {} packages were not installed",
            failed,
            approvable.len()
        );
    }
    Ok(())
}

/// Reject a held item so later syncs do not offer that version, tap or key again.
pub async fn reject(id: &str, expected: Option<&str>) -> Result<()> {
    let Some(item) = reviewed(id, expected, "reject", "Reject this item?")? else {
        return Ok(());
    };
    reject_shown(&item)
}

fn reject_shown(shown: &InboxItem) -> Result<()> {
    let item = inbox::reject(shown)?;
    Output::success(&format!("Rejected {}", describe(&item)));
    Ok(())
}

/// One group's heading: its machine, how many items, and why they wait.
fn group_line(group: &inbox::Group) -> String {
    let reasons: Vec<&str> = group.reasons.iter().map(|r| r.label()).collect();
    format!(
        "{}: {} item{} ({})",
        group
            .machine
            .as_deref()
            .map_or("No machine record".to_string(), |m| format!("From {}", m)),
        group.items.len(),
        if group.items.len() == 1 { "" } else { "s" },
        reasons.join(", ")
    )
}

/// Approve each item one answer covered. A machine key, a malicious package or a failed
/// signature needs its own answer, so callers pass only bulk-approvable items.
async fn approve_each(items: Vec<InboxItem>) {
    for item in items {
        if let Err(e) = approve_locked(&item).await {
            Output::warning(&format!("{}: {}", item.name, e));
        }
    }
}

/// Ask about the held packages, by group, then about each one. Only a terminal user can
/// answer, so callers check for one. The caller holds the sync lock.
pub async fn review_inbox() -> Result<()> {
    // A new machine can inherit hundreds of packages, so one answer can cover a machine's
    // packages, or all of them
    let items = loop {
        let mut items = inbox::list()?;
        if items.len() <= 1 {
            break items;
        }
        inbox::sort_by_group(&mut items);
        Output::section("Packages waiting for approval");
        for group in inbox::groups(&items) {
            Output::list_item(&group_line(&group));
        }
        let mut machines: Vec<&str> = items
            .iter()
            .filter(|i| i.bulk_approvable())
            .filter_map(InboxItem::from_machine)
            .collect();
        machines.dedup();
        let mut options = vec!["Review each".to_string(), "Install all".to_string()];
        options.extend(machines.iter().map(|m| format!("Install all from {}", m)));
        options.push("Decide later".to_string());
        let choice = Prompt::select(
            "Install these packages?",
            options.iter().map(String::as_str).collect(),
            0,
        )?;
        match choice {
            0 => break items,
            1 => {
                approve_each(items.into_iter().filter(|i| i.bulk_approvable()).collect()).await;
                return Ok(());
            }
            c if c == options.len() - 1 => return Ok(()),
            c => {
                // The items shown above, not ones a sync queued since
                let (approvable, _) = inbox::approvable_from(&items, machines[c - 2]);
                approve_each(approvable).await;
            }
        }
    };
    for item in items {
        let options = if item.malicious() {
            vec!["Reject", "Decide later"]
        } else if item.kind != Kind::Package {
            vec!["Trust", "Reject", "Decide later"]
        } else {
            vec!["Install", "Reject", "Decide later"]
        };
        let choice = options[Prompt::select(&describe(&item), options.clone(), options.len() - 1)?];
        let result = match choice {
            "Install" if item.signature_failed() => match confirm_signature_failed(&item) {
                Ok(true) => approve_locked(&item).await,
                other => other.map(|_| ()),
            },
            "Install" | "Trust" => approve_locked(&item).await,
            "Reject" => reject_shown(&item),
            _ => Ok(()),
        };
        if let Err(e) = result {
            Output::warning(&format!("{}: {}", item.name, e));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::inbox::Reason;

    #[test]
    fn approve_requires_the_reviewed_version_tap_or_key() {
        let mut item = InboxItem {
            kind: Kind::Package,
            manager: "npm".to_string(),
            name: "example".to_string(),
            version: Some("1.0.0".to_string()),
            tap: None,
            source_machine: None,
            commit: None,
            signer: None,
            reasons: vec![Reason::Unsigned],
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        };
        assert!(check_expected(&item, "1.0.0").is_ok());
        // A sync replaced the version under the same id
        item.version = Some("1.0.1".to_string());
        assert!(check_expected(&item, "1.0.0").is_err());

        item.manager = "brew_formulae".to_string();
        item.version = None;
        item.tap = Some("evil/tap".to_string());
        assert!(check_expected(&item, "good/tap").is_err());
        assert!(check_expected(&item, "evil/tap").is_ok());

        item.kind = Kind::TrustMachine {
            public_key: String::new(),
            fingerprint: "SHA256:new".to_string(),
        };
        assert!(check_expected(&item, "SHA256:old").is_err());
        assert!(check_expected(&item, "SHA256:new").is_ok());
    }
}
