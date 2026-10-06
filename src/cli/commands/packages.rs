use anyhow::Result;

use crate::cli::output::Output;
use crate::cli::prompts::Prompt;
use crate::packages::inbox::{self, InboxItem, Kind};
use crate::packages::{
    BrewManager, BunManager, GemManager, NpmManager, PackageInfo, PackageManager, PnpmManager,
    UvManager,
};
use crate::sync::membership::{self, Edit, Membership};

struct PackageEntry {
    manager: String,
    name: String,
    version: Option<String>,
}

struct ManagerInfo {
    name: String,
    display: String,
    packages: Vec<PackageInfo>,
}

pub async fn run(list_only: bool, yes: bool) -> Result<()> {
    let managers: Vec<Box<dyn PackageManager>> = vec![
        Box::new(BrewManager::new()),
        Box::new(NpmManager::new()),
        Box::new(PnpmManager::new()),
        Box::new(BunManager::new()),
        Box::new(GemManager::new()),
        Box::new(UvManager::new()),
    ];

    // Collect packages grouped by manager
    let mut manager_infos: Vec<ManagerInfo> = Vec::new();

    for manager in &managers {
        if !manager.is_available().await {
            continue;
        }

        match manager.list_installed().await {
            Ok(packages) => {
                if !packages.is_empty() {
                    manager_infos.push(ManagerInfo {
                        name: manager.name().to_string(),
                        display: format!("{} ({} packages)", manager.name(), packages.len()),
                        packages,
                    });
                }
            }
            Err(e) => {
                Output::warning(&format!(
                    "Failed to list {} packages: {}",
                    manager.name(),
                    e
                ));
            }
        }
    }

    print_install_failures();

    if manager_infos.is_empty() {
        Output::info("No packages found");
        return Ok(());
    }

    manager_infos.sort_by(|a, b| a.name.cmp(&b.name));

    if list_only {
        print_package_list(&manager_infos);
        return Ok(());
    }

    // Interactive mode: first select managers to expand
    let option_refs: Vec<&str> = manager_infos.iter().map(|m| m.display.as_str()).collect();

    let selected_indices =
        match Prompt::multi_select("Select package managers to expand:", option_refs, &[]) {
            Ok(indices) => indices,
            Err(_) => return Ok(()),
        };

    if selected_indices.is_empty() {
        Output::info("No managers selected");
        return Ok(());
    }

    // Build package list from selected managers only
    let mut all_packages: Vec<PackageEntry> = Vec::new();
    for &idx in &selected_indices {
        let info = &manager_infos[idx];
        for pkg in &info.packages {
            all_packages.push(PackageEntry {
                manager: info.name.clone(),
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
        }
    }

    all_packages.sort_by(|a, b| (&a.manager, &a.name).cmp(&(&b.manager, &b.name)));

    // Now select packages to uninstall
    let options: Vec<String> = all_packages
        .iter()
        .map(|p| {
            let version = p.version.as_deref().unwrap_or("");
            if version.is_empty() {
                format!("[{}] {}", p.manager, p.name)
            } else {
                format!("[{}] {} ({})", p.manager, p.name, version)
            }
        })
        .collect();

    let option_refs: Vec<&str> = options.iter().map(|s| s.as_str()).collect();

    let selected = match Prompt::multi_select("Select packages to uninstall:", option_refs, &[]) {
        Ok(indices) => indices,
        Err(_) => return Ok(()),
    };

    if selected.is_empty() {
        Output::info("No packages selected");
        return Ok(());
    }

    // Process each selected package
    for idx in selected {
        let pkg = &all_packages[idx];
        uninstall_package(&managers, pkg, yes).await?;
    }

    Output::success("Uninstall complete");
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

fn print_package_list(manager_infos: &[ManagerInfo]) {
    let membership = crate::config::Config::load()
        .and_then(|config| Membership::load_current(&config))
        .ok();
    if let Some(m) = &membership {
        Output::info(&format!(
            "This machine installs the packages of profile {}",
            m.profile
        ));
    }
    for info in manager_infos {
        Output::section(&info.name);
        let key = if info.name == "brew" {
            "brew_formulae"
        } else {
            info.name.as_str()
        };
        for pkg in &info.packages {
            let mut display = match &pkg.version {
                Some(v) => format!("{} ({})", pkg.name, v),
                None => pkg.name.clone(),
            };
            if let Some(m) = &membership {
                display.push_str(&format!("  [{}]", members_label(m, key, &pkg.name)));
            }
            Output::list_item(&display);
        }
    }
    println!();
}

/// The package's member profiles, or "this profile only".
pub fn members_label(membership: &Membership, manager: &str, name: &str) -> String {
    let members = membership.members(manager, name);
    if members.len() == 1 && members.contains(&membership.profile) {
        "this profile only".to_string()
    } else {
        members.into_iter().collect::<Vec<_>>().join(", ")
    }
}

const MANAGER_KEYS: &[&str] = &[
    "brew_formulae",
    "brew_casks",
    "brew_taps",
    "npm",
    "pnpm",
    "bun",
    "gem",
    "uv",
];

fn split_id(id: &str) -> Result<(&str, &str)> {
    match id.split_once(':') {
        Some((manager, name)) if MANAGER_KEYS.contains(&manager) && !name.is_empty() => {
            Ok((manager, name))
        }
        _ => anyhow::bail!(
            "Name the package as manager:name, such as npm:typescript or brew_casks:zoom. \
             Managers: {}",
            MANAGER_KEYS.join(", ")
        ),
    }
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
    let members = membership::save_edit(&config, manager, name, &edit)?.unwrap_or_default();
    Output::success(&format!(
        "{} now belongs to: {}. Machines in these profiles install it on their next sync",
        membership::canonical_id(manager, name),
        members.into_iter().collect::<Vec<_>>().join(", ")
    ));
    Ok(())
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
            "Profile {} no longer installs {}. Profiles {} keep it",
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

async fn uninstall_package(
    managers: &[Box<dyn PackageManager>],
    pkg: &PackageEntry,
    yes: bool,
) -> Result<()> {
    let manager = managers
        .iter()
        .find(|m| m.name() == pkg.manager)
        .ok_or_else(|| anyhow::anyhow!("Manager {} not found", pkg.manager))?;

    // Check for dependents
    let dependents = manager.get_dependents(&pkg.name).await.unwrap_or_default();

    if !dependents.is_empty() {
        Output::warning(&format!(
            "{} is required by: {}",
            pkg.name,
            dependents.join(", ")
        ));

        if !yes && !Prompt::confirm(&format!("Uninstall {} anyway?", pkg.name), false)? {
            Output::dim(&format!("Skipped {}", pkg.name));
            return Ok(());
        }
    }

    // Uninstall
    match manager.uninstall(&pkg.name).await {
        Ok(()) => {
            Output::success(&format!("Uninstalled {} ({})", pkg.name, pkg.manager));
        }
        Err(e) => {
            Output::error(&format!("Failed to uninstall {}: {}", pkg.name, e));
        }
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
pub async fn inbox_list() -> Result<()> {
    let items = inbox::list()?;
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
        "Run 'tether packages approve <id> <version, tap or key>' or 'tether packages reject <id> <version, tap or key>'",
    );
    Ok(())
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
        None if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
            Output::info(&describe(&item));
            if !Prompt::confirm(question, false)? {
                return Ok(None);
            }
        }
        None => {
            if let Some(binding) = binding(&item) {
                anyhow::bail!(
                    "Check {}, then run 'tether packages {} {} {}'",
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
    Prompt::confirm("Approve it anyway?", false)
}

/// Approve exactly the item shown to the user. The caller holds the sync lock.
async fn approve_locked(shown: &InboxItem) -> Result<()> {
    let mut version = None;
    if shown.kind == Kind::Package {
        match osv_checked(shown).await? {
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
async fn osv_checked(item: &InboxItem) -> Result<Option<Option<String>>> {
    let e = match inbox::check_osv(&item.manager, &item.name, item.version.as_deref(), true).await {
        Ok(version) => return Ok(Some(version)),
        Err(e) => e,
    };
    let unchecked = match e.downcast::<inbox::OsvUnchecked>() {
        Ok(unchecked) if std::io::IsTerminal::is_terminal(&std::io::stdin()) => unchecked,
        Ok(unchecked) => return Err(unchecked.into()),
        Err(e) => return Err(e),
    };
    Output::warning(&unchecked.to_string());
    if Prompt::confirm("Install it without the malicious-package check?", false)? {
        Ok(Some(unchecked.version))
    } else {
        Ok(None)
    }
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
