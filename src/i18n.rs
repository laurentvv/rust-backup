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
    /// Settings: passphrase.
    pub passphrase: &'static str,
    /// Settings: passphrase help.
    pub passphrase_help: &'static str,
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
    /// Snapshot list heading.
    pub snapshots: &'static str,
    /// Search placeholder for the file browser.
    pub search_files: &'static str,
    /// Select all files.
    pub select_all: &'static str,
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
    /// Button: verify (quick).
    pub verify: &'static str,
    /// Files selected counter.
    pub selected_files: &'static str,
    /// Passphrase required hint.
    pub passphrase_needed: &'static str,
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
    passphrase: "Phrase secrète du dépôt",
    passphrase_help: "Stockée en mémoire pour la session uniquement — jamais affichée ni journalisée.",
    catalog_location: "Base de suivi locale",
    no_jobs: "Aucun job configuré — créez-en un ci-dessous.",
    no_history: "Aucune exécution enregistrée pour l'instant.",
    run_title: "Exécution en cours",
    restore: "Restauration",
    snapshots: "Instantanés",
    search_files: "Rechercher un fichier…",
    select_all: "Tout sélectionner",
    to_original: "Emplacement d'origine",
    to_target: "Dossier de destination",
    overwrite_files: "Écraser les fichiers existants",
    restore_now: "Restaurer la sélection",
    restored: "Restauration terminée",
    restore_failed: "Échec de la restauration",
    no_snapshots: "Aucun instantané — lancez d'abord une sauvegarde (et définissez la phrase secrète dans Paramètres).",
    verify: "Vérifier",
    selected_files: "fichiers sélectionnés",
    passphrase_needed: "Définissez la phrase secrète dans Paramètres.",
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
    passphrase: "Repository passphrase",
    passphrase_help: "Kept in memory for the session only — never shown or logged.",
    catalog_location: "Local tracking database",
    no_jobs: "No job configured — create one below.",
    no_history: "No run recorded yet.",
    run_title: "Run in progress",
    restore: "Restore",
    snapshots: "Snapshots",
    search_files: "Search a file…",
    select_all: "Select all",
    to_original: "Original location",
    to_target: "Destination folder",
    overwrite_files: "Overwrite existing files",
    restore_now: "Restore selection",
    restored: "Restore complete",
    restore_failed: "Restore failed",
    no_snapshots: "No snapshot yet — run a backup first (and set the passphrase in Settings).",
    verify: "Verify",
    selected_files: "files selected",
    passphrase_needed: "Set the passphrase in Settings.",
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

/// Human-readable byte size (SI).
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["o", "Kio", "Mio", "Gio", "Tio", "Pio"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
