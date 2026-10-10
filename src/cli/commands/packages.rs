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
pub async fn list(json: bool, other_profiles: bool) -> Result<()> {
    if other_profiles {
        return list_other_profiles(json);
    }
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

/// `packages list --other-profiles`: the packages a sync names in its one-time notice.
/// The JSON has `profile` and `packages`, each with `id`, `manager`, `name` and `profiles`.
fn list_other_profiles(json: bool) -> Result<()> {
    // Without a sync repo or a readable profiles table, as for `list`, there is nothing to show
    let membership =
        match crate::config::Config::load().and_then(|config| Membership::load_current(&config)) {
            Ok(m) => m,
            Err(e) => {
                if json {
                    return Output::json(&serde_json::json!({ "profile": null, "packages": [] }));
                }
                Output::info(&format!("Nothing to show: {}", e));
                return Ok(());
            }
        };
    let excluded: Vec<(String, String, Vec<String>)> = membership
        .excluded_packages()
        .into_iter()
        .map(|(manager, name)| {
            let profiles = membership.members(&manager, &name).into_iter().collect();
            (manager, name, profiles)
        })
        .collect();
    if json {
        let packages: Vec<serde_json::Value> = excluded
            .iter()
            .map(|(manager, name, profiles)| {
                serde_json::json!({
                    "id": format!("{}:{}", manager, name),
                    "manager": manager,
                    "name": name,
                    "profiles": profiles,
                })
            })
            .collect();
        return Output::json(&serde_json::json!({
            "profile": membership.profile,
            "packages": packages,
        }));
    }
    if excluded.is_empty() {
        Output::info(&format!(
            "This machine (profile {}) installs every package that trusted machines list",
            membership.profile
        ));
        return Ok(());
    }
    Output::info(&format!(
        "Packages of other profiles that this machine (profile {}) does not install",
        membership.profile
    ));
    let mut manager = "";
    for (m, name, profiles) in &excluded {
        if m != manager {
            Output::section(m);
            manager = m;
        }
        Output::list_item(&format!("{}  ({})", name, profiles.join(", ")));
    }
    println!();
    Output::dim(&format!(
        "Run 'tether packages share <manager:name> --to {}' to install one here",
        membership.profile
    ));
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

/// Install a package another machine lists, as the dashboard's Import does. Nothing is
/// checked against trusted records, but every other gate of a sync applies, under the sync
/// lock: see [`inbox::check_manual_install`].
pub async fn install(id: &str) -> Result<()> {
    let (manager, name) = split_id(id)?;
    let canonical = membership::canonical_id(manager, name);
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
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
    let sources: Vec<&str> = records
        .iter()
        .filter(|m| m.machine_id != machine_id && lists(m))
        .map(|m| m.machine_id.as_str())
        .collect();
    if sources.is_empty() {
        anyhow::bail!("No other machine lists {}", id);
    }
    if installed_here(manager, name).await? {
        anyhow::bail!("{} is already installed here", id);
    }
    let checked = match inbox::check_manual_install(manager, name, true).await {
        Ok(checked) => checked,
        Err(e) => match e.downcast::<inbox::OsvUnchecked>() {
            Ok(unchecked) if Prompt::is_interactive() && !Prompt::assume_yes() => {
                Output::warning(&unchecked.to_string());
                if !Prompt::question("Install it without the malicious-package check?", false)? {
                    return Ok(());
                }
                inbox::check_manual_install(manager, name, false).await?
            }
            Ok(unchecked) => return Err(unchecked.into()),
            Err(e) => return Err(e),
        },
    };
    Output::info(&format!(
        "Installing {}{} from {}",
        canonical,
        checked
            .version
            .as_deref()
            .map(|v| format!(" {}", v))
            .unwrap_or_default(),
        sources.join(", ")
    ));
    // A cask that needs a password fails without a terminal instead of waiting
    inbox::install_from_machine(manager, name, checked, Prompt::is_interactive()).await?;
    Output::success(&format!("Installed {}", canonical));
    Ok(())
}

/// Whether the manager lists the package as installed now. This machine's record lists it
/// only after the next sync.
async fn installed_here(manager: &str, name: &str) -> Result<bool> {
    let brew = BrewManager::new();
    let names: Vec<String> = match manager {
        "brew_formulae" | "brew_casks" | "brew_taps" if !brew.is_available().await => Vec::new(),
        "brew_formulae" => brew
            .list_installed()
            .await?
            .into_iter()
            .map(|p| p.name)
            .collect(),
        "brew_casks" => brew.list_installed_casks().await?,
        "brew_taps" => brew.list_taps().await?,
        key => match crate::packages::manager_for_key(key) {
            Some(m) if m.is_available().await => m
                .list_installed()
                .await?
                .into_iter()
                .map(|p| p.name)
                .collect(),
            _ => Vec::new(),
        },
    };
    let canonical = membership::canonical_id(manager, name);
    Ok(names
        .iter()
        .any(|n| membership::canonical_id(manager, n) == canonical))
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
    // No sync may save the record between the uninstall and the profile save
    let _lock = crate::sync::acquire_sync_lock(true)?;
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
    match membership::save_edit(&config, manager, name, &edit) {
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
        "expect": item.binding(),
        "bulk_approvable": item.bulk_approvable(),
        "first_seen": item.first_seen,
    })
}

fn check_expected(item: &InboxItem, binding: Option<&str>, expected: &str) -> Result<()> {
    match binding {
        Some(binding) if binding == expected => Ok(()),
        Some(binding) => anyhow::bail!(
            "{} is now {}, not {}. Review it with 'tether packages inbox'",
            item.id(),
            binding,
            expected
        ),
        None => anyhow::bail!(
            "{} has no version, tap or key to name, so --expect cannot bind it. Review it in a \
             terminal",
            item.id()
        ),
    }
}

/// An inbox item as the user reviewed it, and the version that installs. An unpinned
/// package binds to the release it would install now, which the review shows.
struct Reviewed {
    item: InboxItem,
    version: Option<String>,
}

/// The item `id` as the user reviewed it: shown and confirmed in a terminal, or checked
/// against the version, tap or key they named. So a replacement that a sync queued under
/// the same id is not decided on. `-y` never answers the review. An item without anything
/// to name needs a terminal. None when the user declines.
async fn reviewed(
    id: &str,
    expected: Option<&str>,
    action: &str,
    question: &str,
) -> Result<Option<Reviewed>> {
    let item = inbox::Inbox::load()?.find(id)?.clone();
    // Only an approval installs, so only an approval binds the release that installs
    let resolved = match (&item.kind, &item.version) {
        (Kind::Package, None) if action == "approve" => {
            inbox::release_to_install(&item.manager, &item.name).await
        }
        _ => None,
    };
    let binding = item.binding().or(resolved.as_deref()).map(str::to_string);
    match expected {
        Some(expected) => check_expected(&item, binding.as_deref(), expected)?,
        None if Prompt::is_interactive() => {
            Output::info(&describe(&item));
            match (&resolved, binding.is_some()) {
                (Some(version), _) => Output::info(&format!("Installs version {}", version)),
                (None, false) => Output::warning(&format!(
                    "{} has no version or tap, so Tether cannot show what installs",
                    item.id()
                )),
                (None, true) => {}
            }
            if !Prompt::review(question)? {
                return Ok(None);
            }
        }
        None => match binding {
            Some(binding) => anyhow::bail!(
                "Check {}, then run 'tether packages {} {} --expect {}'",
                describe(&item),
                action,
                item.id(),
                binding
            ),
            None => anyhow::bail!(
                "{} has no version, tap or key to name, so it needs a review in a terminal. \
                 Run 'tether packages {} {}' in a terminal",
                describe(&item),
                action,
                item.id()
            ),
        },
    }
    let version = item.version.clone().or(resolved);
    Ok(Some(Reviewed { item, version }))
}

/// Approve a held package and install it now, or trust a held machine key. The sync lock
/// keeps the daemon from installing the same package while this install runs.
pub async fn approve(id: &str, expected: Option<&str>, allow_signature_failed: bool) -> Result<()> {
    if let Some(machine) = id.strip_prefix("machine:") {
        return super::machines::trust(machine, expected).await;
    }
    let Some(Reviewed { item, version }) =
        reviewed(id, expected, "approve", "Approve this item?").await?
    else {
        return Ok(());
    };
    if item.signature_failed() {
        if Prompt::is_interactive() {
            if !confirm_signature_failed(&item)? {
                return Ok(());
            }
        } else if !allow_signature_failed {
            anyhow::bail!(
                "{} comes from {}, whose record fails its signature. Approve it in a terminal, \
                 or pass --allow-signature-failed with --expect",
                item.id(),
                item.source_machine.as_deref().unwrap_or("another machine")
            );
        }
    }
    let _sync_lock = crate::sync::acquire_sync_lock(true)?;
    Output::info(&format!("Approving {}", describe(&item)));
    approve_locked(&item, version.as_deref()).await
}

/// A package whose source machine's record fails its signature may be a forged manifest
/// line, so the user types its version or tap and answers a second question. `-y` answers
/// neither.
fn confirm_signature_failed(item: &InboxItem) -> Result<bool> {
    Output::warning(&format!(
        "{} comes from {}, whose record fails its signature. Someone may have edited it in \
         the repo. Do not approve it unless you know why the signature fails",
        item.name,
        item.source_machine.as_deref().unwrap_or("another machine")
    ));
    if let Some(binding) = item.binding() {
        if Prompt::input(&format!("Type {} to approve it", binding), None)?.trim() != binding {
            Output::info(&format!("Not approved: {}", item.id()));
            return Ok(false);
        }
    }
    Prompt::review("Approve it anyway?")
}

/// Approve exactly the item shown to the user, and install `version`, the release the
/// review bound. The caller holds the sync lock.
async fn approve_locked(shown: &InboxItem, version: Option<&str>) -> Result<()> {
    let mut version = version.map(str::to_string);
    if shown.kind == Kind::Package {
        match osv_checked(&shown.manager, &shown.name, version.as_deref()).await? {
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
        Prompt::is_interactive(),
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
    let mut items = inbox::list()?;
    let from = match from {
        // An item names its machine by id, also when that machine has no record any more
        Some(m) if items.iter().any(|i| i.from_machine() == Some(m)) => Some(m.to_string()),
        Some(m) => Some(super::machines::resolve(
            &crate::sync::SyncEngine::sync_path()?,
            m,
        )?),
        None => None,
    };
    inbox::sort_by_group(&mut items);
    let (approvable, held): (Vec<InboxItem>, Vec<InboxItem>) = items
        .into_iter()
        .filter(|i| from.as_deref().is_none_or(|m| i.from_machine() == Some(m)))
        .partition(InboxItem::bulk_approvable);
    if !held.is_empty() {
        Output::info(&format!(
            "{} item(s) need their own decision and stay in the inbox: machine keys, packages \
             OSV lists as malicious, packages whose record fails its signature, and packages \
             without a version or tap to name",
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
        if let Err(e) = approve_locked(item, item.version.as_deref()).await {
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
    let Some(Reviewed { item, .. }) = reviewed(id, expected, "reject", "Reject this item?").await?
    else {
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
        if let Err(e) = approve_locked(&item, item.version.as_deref()).await {
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
                Ok(true) => approve_locked(&item, item.version.as_deref()).await,
                other => other.map(|_| ()),
            },
            "Install" | "Trust" => approve_locked(&item, item.version.as_deref()).await,
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
        let check = |item: &InboxItem, expected| check_expected(item, item.binding(), expected);
        assert!(check(&item, "1.0.0").is_ok());
        // A sync replaced the version under the same id
        item.version = Some("1.0.1".to_string());
        assert!(check(&item, "1.0.0").is_err());

        item.manager = "brew_formulae".to_string();
        item.version = None;
        item.tap = Some("evil/tap".to_string());
        assert!(check(&item, "good/tap").is_err());
        assert!(check(&item, "evil/tap").is_ok());

        item.kind = Kind::TrustMachine {
            public_key: String::new(),
            fingerprint: "SHA256:new".to_string(),
        };
        assert!(check(&item, "SHA256:old").is_err());
        assert!(check(&item, "SHA256:new").is_ok());
    }

    #[test]
    fn an_item_without_a_binding_needs_its_own_review() {
        let mut item = InboxItem {
            kind: Kind::Package,
            manager: "brew_taps".to_string(),
            name: "x/y".to_string(),
            version: None,
            tap: None,
            source_machine: Some("other".to_string()),
            commit: None,
            signer: None,
            reasons: vec![Reason::UntrustedTap],
            advisories: Vec::new(),
            first_seen: chrono::Utc::now(),
        };
        // A tap binds to its own name
        assert_eq!(item.binding(), Some("x/y"));
        assert!(item.bulk_approvable());

        // An unpinned package binds only to the release a review resolves
        item.manager = "npm".to_string();
        item.name = "held".to_string();
        item.reasons = vec![Reason::Unsigned];
        assert_eq!(item.binding(), None);
        assert!(!item.bulk_approvable());
        assert!(check_expected(&item, None, "1.0.0")
            .unwrap_err()
            .to_string()
            .contains("Review it in a terminal"));
        assert!(check_expected(&item, Some("1.0.1"), "1.0.0").is_err());
        assert!(check_expected(&item, Some("1.0.0"), "1.0.0").is_ok());
    }
}
