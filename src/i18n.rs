//! UI strings: French (default) and English.

/// Which language the UI displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// Français (défaut).
    #[default]
    Fr,
    /// English.
    En,
}

/// All UI strings for one language.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Texts {
    /// Section: dashboard.
    pub dashboard: &'static str,
    /// Section: jobs.
    pub jobs: &'static str,
    /// Section: run view.
    pub run: &'static str,
    /// Section: history.
    pub history: &'static str,
    /// Section: settings.
    pub settings: &'static str,
    /// Navigation group label.
    pub navigation: &'static str,
    /// App tagline.
    pub tagline: &'static str,
    /// Stat card: configured jobs.
    pub jobs_count: &'static str,
    /// Stat card: snapshots.
    pub snapshots_count: &'static str,
    /// Stat card: last run.
    pub last_run: &'static str,
    /// Stat card: protected volume.
    pub protected_volume: &'static str,
    /// New job form title.
    pub new_job: &'static str,
    /// Field: job name.
    pub name: &'static str,
    /// Field: source directory.
    pub source: &'static str,
    /// Field: local destination.
    pub local_root: &'static str,
    /// Button: create.
    pub create: &'static str,
    /// Button: run now.
    pub run_now: &'static str,
    /// Running state.
    pub running: &'static str,
    /// Idle state.
    pub idle: &'static str,
    /// Cancel button.
    pub cancel: &'static str,
    /// Cancelled state.
    pub cancelled: &'static str,
    /// Cancelled by user reason.
    pub cancelled_by_user: &'static str,
    /// Files processed.
    pub files_done: &'static str,
    /// New chunks stored.
    pub new_chunks: &'static str,
    /// Data processed.
    pub bytes_processed: &'static str,
    /// Success banner.
    pub backup_done: &'static str,
    /// Failure banner.
    pub backup_failed: &'static str,
    /// Skipped files.
    pub skipped: &'static str,
    /// History: status.
    pub status: &'static str,
    /// Success.
    pub success: &'static str,
    /// Failure.
    pub failed: &'static str,
    /// Settings: language.
    pub language: &'static str,
    /// Settings: theme.
    pub theme: &'static str,
    /// Settings: dark theme.
    pub theme_dark: &'static str,
    /// Settings: light theme.
    pub theme_light: &'static str,
    /// Settings: passphrase.
    pub passphrase: &'static str,
    /// Settings: passphrase help.
    pub passphrase_help: &'static str,
    /// Status: passphrase active.
    pub passphrase_set: &'static str,
    /// Status: passphrase not set.
    pub passphrase_unset: &'static str,
    /// Settings: catalog location.
    pub catalog_location: &'static str,
    /// Empty list message.
    pub no_jobs: &'static str,
    /// No history message.
    pub no_history: &'static str,
    /// Title of the run section.
    pub run_title: &'static str,
    /// Section: restore.
    pub restore: &'static str,
    /// Select a job.
    pub select_job: &'static str,
    /// Snapshot list heading.
    pub snapshots: &'static str,
    /// Refresh button.
    pub refresh: &'static str,
    /// Search placeholder for the file browser.
    pub search_files: &'static str,
    /// Select all files.
    pub select_all: &'static str,
    /// Deselect all files.
    pub deselect_all: &'static str,
    /// Word: files.
    pub files: &'static str,
    /// Word: directories.
    pub directories: &'static str,
    /// Restore to the original location.
    pub to_original: &'static str,
    /// Restore under a chosen directory.
    pub to_target: &'static str,
    /// Overwrite existing files.
    pub overwrite_files: &'static str,
    /// Button: restore now.
    pub restore_now: &'static str,
    /// Success banner for restore.
    pub restored: &'static str,
    /// Failure banner for restore.
    pub restore_failed: &'static str,
    /// Empty snapshot list.
    pub no_snapshots: &'static str,
    /// Restore page: snapshots exist but the passphrase is not set yet.
    pub snapshots_need_passphrase: &'static str,
    /// Banner shown when the local catalog cannot be read.
    pub catalog_unreadable: &'static str,
    /// History table: date column header.
    pub date_col: &'static str,
    /// Button: verify (quick).
    pub verify: &'static str,
    /// Files selected counter.
    pub selected_files: &'static str,
    /// Passphrase required hint.
    pub passphrase_needed: &'static str,
    /// Destination type label.
    pub destination_type: &'static str,
    /// Destination type: local.
    pub local: &'static str,
    /// Destination type: SFTP.
    pub sftp: &'static str,
    /// SFTP host field.
    pub sftp_host: &'static str,
    /// SFTP port field.
    pub sftp_port: &'static str,
    /// SFTP username field.
    pub sftp_user: &'static str,
    /// SFTP private key field.
    pub sftp_key: &'static str,
    /// SFTP remote directory field.
    pub sftp_root: &'static str,
    /// Browse button.
    pub browse: &'static str,
    /// Excludes input label.
    pub excludes: &'static str,
    /// Excludes placeholder/help.
    pub excludes_help: &'static str,
    /// Compression level label.
    pub compression: &'static str,
    /// Edit job button.
    pub edit: &'static str,
    /// Delete job button.
    pub delete: &'static str,
    /// Save job button.
    pub save: &'static str,
    /// Job updated toast.
    pub job_updated: &'static str,
    /// Job deleted toast.
    pub job_deleted: &'static str,
    /// Confirm delete title.
    pub confirm_delete: &'static str,
    /// Confirm delete description.
    pub confirm_delete_desc: &'static str,
    /// Initialize repository button.
    pub init_repo: &'static str,
    /// Repository initialized toast.
    pub repo_initialized: &'static str,
    /// Run page: destination holds no repository (banner).
    pub repo_missing: &'static str,
    /// Run page: explanation before the explicit init confirmation.
    pub repo_missing_desc: &'static str,
    /// Run page: confirm init then back up.
    pub init_and_backup: &'static str,
    /// Quick verification option.
    pub verify_quick: &'static str,
    /// Full verification option.
    pub verify_full: &'static str,
    /// Verification mode label.
    pub verify_mode: &'static str,
    /// Verification running banner/toast.
    pub verify_running: &'static str,
    /// Verification cancelled.
    pub verify_cancelled: &'static str,
    /// Backup history tab.
    pub backup_history: &'static str,
    /// Verification history tab.
    pub verify_history: &'static str,
    /// No verification history.
    pub no_verify_history: &'static str,
    /// Integrity OK.
    pub integrity_ok: &'static str,
    /// Integrity issues found.
    pub issues_found: &'static str,
    /// Chunks checked label.
    pub chunks_checked: &'static str,
    /// Orphan chunks label.
    pub orphan_chunks: &'static str,
    /// Tree view button.
    pub tree_view: &'static str,
    /// Flat view button.
    pub flat_view: &'static str,
    /// Expand all button.
    pub expand_all: &'static str,
    /// Collapse all button.
    pub collapse_all: &'static str,
    /// Restoring progress.
    pub restoring: &'static str,
    /// Cancel restore button.
    pub cancel_restore: &'static str,
    /// Remaining-time label of the restore progress ("ETA").
    pub time_left: &'static str,
    /// Restore cancelled.
    pub restore_cancelled: &'static str,
    /// Prune button.
    pub prune: &'static str,
    /// Pruning in progress.
    pub pruning: &'static str,
    /// Prune complete.
    pub prune_done: &'static str,
    /// Prune confirmation dialog title.
    pub confirm_prune: &'static str,
    /// Prune confirmation dialog description.
    pub confirm_prune_desc: &'static str,
    /// Delete snapshot.
    pub delete_snapshot: &'static str,
    /// Confirm delete snapshot.
    pub confirm_delete_snapshot: &'static str,
    /// Confirm delete snapshot description.
    pub confirm_delete_snapshot_desc: &'static str,
    /// Snapshot deleted toast.
    pub snapshot_deleted: &'static str,
    /// Cadence: max incrementals before full.
    pub max_incrementals: &'static str,
    /// Cadence help text.
    pub max_incrementals_hint: &'static str,
    /// Mode: full.
    pub mode_full: &'static str,
    /// Mode: incremental.
    pub mode_incremental: &'static str,
    /// Force full backup toggle.
    pub force_full: &'static str,
    /// Cadence badge.
    pub cadence_badge: &'static str,
}

/// French strings.
pub const FR: Texts = Texts {
    dashboard: "Tableau de bord",
    jobs: "Sauvegardes",
    run: "Exécution",
    history: "Historique",
    settings: "Paramètres",
    navigation: "Navigation",
    tagline: "Sauvegardes chiffrées, dédupliquées — disque USB ou serveur SFTP",
    jobs_count: "Jobs configurés",
    snapshots_count: "Instantanés",
    last_run: "Dernière exécution",
    protected_volume: "Volume protégé",
    new_job: "Nouveau job",
    name: "Nom",
    source: "Répertoire source",
    local_root: "Destination locale (USB…)",
    create: "Créer",
    run_now: "Sauvegarder",
    running: "en cours",
    idle: "inactif",
    cancel: "Annuler",
    cancelled: "Sauvegarde annulée",
    cancelled_by_user: "Annulé par l'utilisateur",
    files_done: "Fichiers traités",
    new_chunks: "Nouveaux blocs",
    bytes_processed: "Données traitées",
    backup_done: "Sauvegarde finalisée",
    backup_failed: "Échec de la sauvegarde",
    skipped: "ignorés",
    status: "Statut",
    success: "Succès",
    failed: "Échec",
    language: "Langue",
    theme: "Thème",
    theme_dark: "Sombre",
    theme_light: "Clair",
    passphrase: "Phrase secrète du dépôt",
    passphrase_help: "Stockée en mémoire pour la session uniquement — jamais affichée ni journalisée.",
    passphrase_set: "Phrase secrète en mémoire",
    passphrase_unset: "Phrase secrète non définie",
    catalog_location: "Base de suivi locale",
    no_jobs: "Aucun job configuré — créez-en un ci-dessous.",
    no_history: "Aucune exécution enregistrée pour l'instant.",
    run_title: "Exécution en cours",
    restore: "Restauration",
    select_job: "Sélectionner un job",
    snapshots: "Instantanés",
    refresh: "Actualiser",
    search_files: "Rechercher un fichier…",
    select_all: "Tout sélectionner",
    deselect_all: "Tout désélectionner",
    files: "fichiers",
    directories: "répertoires",
    to_original: "Emplacement d'origine",
    to_target: "Dossier de destination",
    overwrite_files: "Écraser les fichiers existants",
    restore_now: "Restaurer la sélection",
    restored: "Restauration terminée",
    restore_failed: "Échec de la restauration",
    no_snapshots: "Aucun instantané — lancez d'abord une sauvegarde (et définissez la phrase secrète dans Paramètres).",
    snapshots_need_passphrase: "Des instantanés existent : définissez la phrase secrète dans Paramètres, puis cliquez sur Actualiser.",
    catalog_unreadable: "Catalogue illisible (verrouillé par un autre processus ?) : nouvelle tentative automatique.",
    date_col: "Date",
    verify: "Vérifier",
    selected_files: "fichiers sélectionnés",
    passphrase_needed: "Définissez la phrase secrète dans Paramètres.",
    destination_type: "Type de destination",
    local: "Disque local / USB",
    sftp: "Serveur SFTP",
    sftp_host: "Hôte SFTP (ex. backup.example.com)",
    sftp_port: "Port (défaut 22)",
    sftp_user: "Utilisateur SSH",
    sftp_key: "Clé privée SSH (id_ed25519…)",
    sftp_root: "Dépôt distant (ex. /data/backup)",
    browse: "Parcourir…",
    excludes: "Exclusions (motifs séparés par des virgules)",
    excludes_help: "target, node_modules, .git, *.tmp",
    compression: "Niveau de compression zstd",
    edit: "Modifier",
    delete: "Supprimer",
    save: "Enregistrer",
    job_updated: "Configuration mise à jour",
    job_deleted: "Tâche supprimée",
    confirm_delete: "Confirmer la suppression",
    confirm_delete_desc: "Supprimer définitivement cette tâche ? Les sauvegardes existantes restent intactes.",
    init_repo: "Initialiser le dépôt",
    repo_initialized: "Dépôt initialisé avec succès",
    repo_missing: "Aucun dépôt à cette destination",
    repo_missing_desc: "Rien n'a été écrit. S'il s'agit d'un disque amovible, vérifiez qu'il est branché et qu'il porte bien la même lettre de lecteur. Sinon, confirmez la création d'un nouveau dépôt chiffré à cet emplacement.",
    init_and_backup: "Créer le dépôt ici et sauvegarder",
    verify_quick: "Rapide (métadonnées)",
    verify_full: "Complète (hachage BLAKE3)",
    verify_mode: "Mode de vérification",
    verify_running: "Vérification d'intégrité en cours…",
    verify_cancelled: "Vérification annulée",
    backup_history: "Sauvegardes",
    verify_history: "Vérifications d'intégrité",
    no_verify_history: "Aucune vérification enregistrée.",
    integrity_ok: "Intégrité validée",
    issues_found: "Problèmes détectés",
    chunks_checked: "fragments vérifiés",
    orphan_chunks: "fragments orphelins",
    tree_view: "Arborescence",
    flat_view: "Liste",
    expand_all: "Tout déplier",
    collapse_all: "Tout replier",
    restoring: "Restauration en cours…",
    cancel_restore: "Annuler la restauration",
    time_left: "restant",
    restore_cancelled: "Restauration annulée",
    prune: "Nettoyer l'espace (Prune)",
    pruning: "Nettoyage en cours…",
    prune_done: "Nettoyage terminé",
    confirm_prune: "Nettoyer le dépôt",
    confirm_prune_desc: "Supprimer définitivement de la destination les blocs qui ne sont plus référencés par aucun instantané ? Les instantanés existants restent intacts ; l'espace libéré ne peut pas être récupéré.",
    delete_snapshot: "Supprimer",
    confirm_delete_snapshot: "Supprimer l'instantané",
    confirm_delete_snapshot_desc: "Supprimer définitivement cet instantané ? Ses données uniques pourront être purgées lors du nettoyage.",
    snapshot_deleted: "Instantané supprimé",
    max_incrementals: "Périodicité sauvegarde complète (0 = désactivé)",
    max_incrementals_hint: "Nombre d'incrémentales avant 1 sauvegarde complète",
    mode_full: "Complète",
    mode_incremental: "Incrémentale",
    force_full: "Forcer une sauvegarde complète",
    cadence_badge: "Cadence Full",
};

/// English strings.
pub const EN: Texts = Texts {
    dashboard: "Dashboard",
    jobs: "Backups",
    run: "Run",
    history: "History",
    settings: "Settings",
    navigation: "Navigation",
    tagline: "Encrypted, deduplicated backups — USB disk or SFTP server",
    jobs_count: "Configured jobs",
    snapshots_count: "Snapshots",
    last_run: "Last run",
    protected_volume: "Protected volume",
    new_job: "New job",
    name: "Name",
    source: "Source directory",
    local_root: "Local destination (USB…)",
    create: "Create",
    run_now: "Back up now",
    running: "running",
    idle: "idle",
    cancel: "Cancel",
    cancelled: "Backup cancelled",
    cancelled_by_user: "Cancelled by user",
    files_done: "Files processed",
    new_chunks: "New chunks",
    bytes_processed: "Data processed",
    backup_done: "Backup finalized",
    backup_failed: "Backup failed",
    skipped: "skipped",
    status: "Status",
    success: "Success",
    failed: "Failed",
    language: "Language",
    theme: "Theme",
    theme_dark: "Dark",
    theme_light: "Light",
    passphrase: "Repository passphrase",
    passphrase_help: "Kept in memory for the session only — never shown or logged.",
    passphrase_set: "Passphrase active in memory",
    passphrase_unset: "Passphrase not set",
    catalog_location: "Local tracking database",
    no_jobs: "No job configured — create one below.",
    no_history: "No run recorded yet.",
    run_title: "Run in progress",
    restore: "Restore",
    select_job: "Select a job",
    snapshots: "Snapshots",
    refresh: "Refresh",
    search_files: "Search a file…",
    select_all: "Select all",
    deselect_all: "Deselect all",
    files: "files",
    directories: "directories",
    to_original: "Original location",
    to_target: "Destination folder",
    overwrite_files: "Overwrite existing files",
    restore_now: "Restore selection",
    restored: "Restore complete",
    restore_failed: "Restore failed",
    no_snapshots: "No snapshot yet — run a backup first (and set the passphrase in Settings).",
    snapshots_need_passphrase: "Snapshots exist: set the passphrase in Settings, then click Refresh.",
    catalog_unreadable: "Catalog unreadable (locked by another process?): retrying automatically.",
    date_col: "Date",
    verify: "Verify",
    selected_files: "files selected",
    passphrase_needed: "Set the passphrase in Settings.",
    destination_type: "Destination type",
    local: "Local disk / USB",
    sftp: "SFTP server",
    sftp_host: "SFTP host (e.g. backup.example.com)",
    sftp_port: "Port (default 22)",
    sftp_user: "SSH username",
    sftp_key: "SSH private key (id_ed25519…)",
    sftp_root: "Remote repository path (e.g. /data/backup)",
    browse: "Browse…",
    excludes: "Excludes (comma-separated patterns)",
    excludes_help: "target, node_modules, .git, *.tmp",
    compression: "zstd compression level",
    edit: "Edit",
    delete: "Delete",
    save: "Save",
    job_updated: "Job updated",
    job_deleted: "Job deleted",
    confirm_delete: "Confirm deletion",
    confirm_delete_desc: "Permanently delete this job? Existing backups remain intact.",
    init_repo: "Initialize repository",
    repo_initialized: "Repository initialized successfully",
    repo_missing: "No repository at this destination",
    repo_missing_desc: "Nothing was written. If this is a removable disk, check that it is plugged in and still has the same drive letter. Otherwise, confirm creating a new encrypted repository at this location.",
    init_and_backup: "Create repository here and back up",
    verify_quick: "Quick (metadata)",
    verify_full: "Full (BLAKE3 hash)",
    verify_mode: "Verification mode",
    verify_running: "Integrity verification in progress…",
    verify_cancelled: "Verification cancelled",
    backup_history: "Backups",
    verify_history: "Integrity verifications",
    no_verify_history: "No verification recorded yet.",
    integrity_ok: "Integrity verified",
    issues_found: "Issues detected",
    chunks_checked: "chunks checked",
    orphan_chunks: "orphan chunks",
    tree_view: "Tree view",
    flat_view: "Flat list",
    expand_all: "Expand all",
    collapse_all: "Collapse all",
    restoring: "Restoring files…",
    cancel_restore: "Cancel restore",
    time_left: "left",
    restore_cancelled: "Restore cancelled",
    prune: "Prune storage",
    pruning: "Pruning in progress…",
    prune_done: "Prune complete",
    confirm_prune: "Prune repository",
    confirm_prune_desc: "Permanently delete from the destination the chunks no snapshot references anymore? Existing snapshots stay intact; freed data cannot be recovered.",
    delete_snapshot: "Delete",
    confirm_delete_snapshot: "Delete snapshot",
    confirm_delete_snapshot_desc: "Permanently delete this snapshot? Unique chunks will be reclaimed on next prune.",
    snapshot_deleted: "Snapshot deleted",
    max_incrementals: "Full backup cadence (0 = disabled)",
    max_incrementals_hint: "Number of incrementals before 1 full backup",
    mode_full: "Full",
    mode_incremental: "Incremental",
    force_full: "Force full backup",
    cadence_badge: "Full Cadence",
};

impl Lang {
    /// The string table for this language.
    #[must_use]
    pub fn texts(self) -> &'static Texts {
        match self {
            Self::Fr => &FR,
            Self::En => &EN,
        }
    }
}

/// Human-readable byte size with language units.
#[must_use]
pub fn human_bytes_lang(bytes: u64, lang: Lang) -> String {
    let units: [&str; 6] = match lang {
        Lang::Fr => ["o", "Kio", "Mio", "Gio", "Tio", "Pio"],
        Lang::En => ["B", "KiB", "MiB", "GiB", "TiB", "PiB"],
    };
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", units[0])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// Local date and time for tables: `12/11/2026 11:55` (FR) or `2026-11-12 11:55` (EN).
#[must_use]
pub fn format_local(dt: chrono::DateTime<chrono::Utc>, lang: Lang) -> String {
    let local = dt.with_timezone(&chrono::Local);
    match lang {
        Lang::Fr => local.format("%d/%m/%Y %H:%M").to_string(),
        Lang::En => local.format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// Remaining time, compact and language neutral: `1h02m`, `4m05s`, `6s`.
#[must_use]
pub fn format_eta(secs: f64) -> String {
    let total = if secs.is_finite() {
        secs.max(0.0).round() as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Human-readable byte size (default French units for backward compatibility).
#[must_use]
#[allow(dead_code)]
pub fn human_bytes(bytes: u64) -> String {
    human_bytes_lang(bytes, Lang::Fr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_eta_is_compact() {
        assert_eq!(format_eta(6.4), "6s");
        assert_eq!(format_eta(245.0), "4m05s");
        assert_eq!(format_eta(3723.0), "1h02m");
        assert_eq!(format_eta(f64::INFINITY), "0s");
    }

    #[test]
    fn test_human_bytes_formatting() {
        assert_eq!(human_bytes_lang(0, Lang::Fr), "0 o");
        assert_eq!(human_bytes_lang(0, Lang::En), "0 B");
        assert_eq!(human_bytes_lang(500, Lang::Fr), "500 o");
        assert_eq!(human_bytes_lang(1024, Lang::Fr), "1.0 Kio");
        assert_eq!(human_bytes_lang(1024, Lang::En), "1.0 KiB");
        assert_eq!(human_bytes_lang(1024 * 1024 * 5, Lang::En), "5.0 MiB");
        assert_eq!(human_bytes(2048), "2.0 Kio");
    }

    #[test]
    fn test_texts_availability() {
        let fr = Lang::Fr.texts();
        let en = Lang::En.texts();
        assert_eq!(fr.cancel, "Annuler");
        assert_eq!(en.cancel, "Cancel");
        assert_eq!(fr.theme_dark, "Sombre");
        assert_eq!(en.theme_dark, "Dark");
    }
}
