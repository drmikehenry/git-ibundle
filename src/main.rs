use std::collections;
use std::ffi;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path;
use uuid;

use anyhow::{anyhow, bail, Context};
use bstr::{BStr, BString, ByteSlice, ByteVec};
use clap::Parser;
use log::{log_enabled, Level};

type AResult<T> = anyhow::Result<T>;
type SeqNum = u64;
type SeqNums = Vec<SeqNum>;

const STATUS_OK: i32 = 0;
const STATUS_ERROR: i32 = 1;
const STATUS_EMPTY_BUNDLE: i32 = 3;

const IBUNDLE_FORMAT_V2: &[u8] = b"# v2 git ibundle";
const REPO_META_FORMAT_V1: &[u8] = b"# v1 repo meta";
const GIT_BUNDLE_FORMAT_V2: &[u8] = b"# v2 git bundle";

fn quoted<B: AsRef<BStr>>(s: B) -> String {
    let s = s.as_ref();
    if s.is_ascii() && !s.contains(&b'\'') {
        format!("'{}'", s)
    } else {
        format!("{:?}", s)
    }
}

fn quoted_path<P: AsRef<std::path::Path>>(path: P) -> String {
    let p = path.as_ref().display().to_string();
    quoted(p.as_bytes())
}

fn name_to_string(name: impl AsRef<BStr>) -> AResult<String> {
    if let Ok(s) = name.as_ref().to_str() {
        Ok(s.to_string())
    } else {
        bail!("name {} is not valid UTF8", quoted(name));
    }
}

fn open_file<P: AsRef<std::path::Path>>(path: P) -> AResult<fs::File> {
    let path = path.as_ref();
    fs::File::open(path).with_context(|| {
        format!("failed to open file {} for reading", quoted_path(path))
    })
}

fn open_reader<P: AsRef<std::path::Path>>(
    path: P,
) -> AResult<io::BufReader<fs::File>> {
    Ok(io::BufReader::new(open_file(path)?))
}

fn create_file<P: AsRef<std::path::Path>>(path: P) -> AResult<fs::File> {
    let path = path.as_ref();
    fs::File::create(path).with_context(|| {
        format!("failed to create file {} for writing", quoted_path(path))
    })
}

fn create_writer<P: AsRef<std::path::Path>>(
    path: P,
) -> AResult<io::BufWriter<fs::File>> {
    Ok(io::BufWriter::new(create_file(path)?))
}

fn read_bytes_until<R: io::Read, F>(
    reader: &mut io::BufReader<R>,
    bline: &mut Vec<u8>,
    is_terminator: F,
) -> io::Result<usize>
where
    F: Fn(u8) -> bool,
{
    bline.clear();
    let mut read = 0;
    loop {
        let done;
        let used;

        {
            let available = match reader.fill_buf() {
                Ok(available) => available,
                Err(e) => {
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e);
                }
            };

            if let Some(pos) = available.iter().position(|&c| is_terminator(c))
            {
                done = true;
                used = pos + 1;
            } else {
                done = false;
                used = available.len();
            }
            bline.extend_from_slice(&available[..used]);
        }
        reader.consume(used);
        read += used;
        if done || used == 0 {
            return Ok(read);
        }
    }
}

fn read_bline(f: &mut impl io::BufRead, line: &mut BString) -> AResult<usize> {
    line.clear();
    f.read_until(b'\n', line)?;
    if line.ends_with(&[b'\n']) {
        line.pop();
    }
    Ok(line.len())
}

fn bstr_pop_word<'a>(bstr: &'a BStr) -> (&'a BStr, &'a BStr) {
    if let Some((word, rest)) = bstr.split_once_str(b" ") {
        (word.as_bstr(), rest.as_bstr())
    } else {
        (bstr, &bstr[bstr.len()..])
    }
}

fn parse_bool(bstr: &BStr) -> AResult<bool> {
    if bstr == b"true".as_bstr() {
        Ok(true)
    } else if bstr == b"false".as_bstr() {
        Ok(false)
    } else {
        bail!("invalid boolean {}", bstr);
    }
}

fn bool_as_bstr(value: bool) -> &'static BStr {
    if value {
        b"true".as_bstr()
    } else {
        b"false".as_bstr()
    }
}

fn parse_seq_num<S: AsRef<[u8]>>(bstr: S) -> AResult<SeqNum> {
    Ok(bstr.as_ref().to_str_lossy().parse::<SeqNum>()?)
}

fn oid_to_bstring(oid: &git2::Oid) -> BString {
    oid.to_string().into()
}

fn write_oid<T: io::Write>(f: &mut T, oid: &git2::Oid) -> AResult<()> {
    f.write_all(oid.to_string().as_bytes())?;
    Ok(())
}

fn write_bline<T: io::Write>(f: &mut T, bstr: &BStr) -> AResult<()> {
    f.write_all(bstr)?;
    f.write_all(b"\n")?;
    Ok(())
}

fn write_oid_bstr_bline<T: io::Write>(
    f: &mut T,
    oid: &git2::Oid,
    bstr: &BStr,
) -> AResult<()> {
    write_oid(f, oid)?;
    f.write_all(b" ")?;
    write_bline(f, bstr)?;
    Ok(())
}

fn parse_oid(bstr: &BStr) -> AResult<git2::Oid> {
    Ok(git2::Oid::from_str(std::str::from_utf8(bstr)?)?)
}

fn oid_bstr_parse(bstr: &BStr) -> AResult<(git2::Oid, BString)> {
    if bstr.find_byte(b' ').is_some() {
        let (oid_bstr, rest_bstr) = bstr_pop_word(bstr);
        Ok((parse_oid(oid_bstr)?, BString::from(rest_bstr)))
    } else {
        bail!("missing space in {}", bstr);
    }
}

// Deletes `file_path` when `FileDeleter` is dropped.
struct FileDeleter {
    file_path: Option<path::PathBuf>,
}

impl FileDeleter {
    fn new<P: AsRef<path::Path>>(file_path: P) -> Self {
        Self {
            file_path: Some(file_path.as_ref().to_path_buf()),
        }
    }
}

impl Drop for FileDeleter {
    fn drop(&mut self) {
        if let Some(file_path) = self.file_path.take() {
            fs::remove_file(&file_path).ok();
        }
    }
}

//////////////////////////////////////////////////////////////////////////////

/// Git offline incremental mirroring via ibundle files
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(flatten)]
    #[command(next_display_order = 10000)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Args, Debug)]
struct CreateArgs {
    /// ibundle file to create
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,

    /// Choose alternate basis sequence number
    #[arg(long)]
    basis: Option<SeqNum>,

    /// Choose basis to be current repository state
    #[arg(long, conflicts_with("basis"))]
    basis_current: bool,

    /// Force ibundle to be standalone
    #[arg(
        long,
        default_value_if(
            "basis_current",
            clap::builder::ArgPredicate::Equals("true".into()),
            Some("true")
        )
    )]
    standalone: bool,

    /// Allow creation of an empty ibundle
    #[arg(
        long,
        default_value_if(
            "basis_current",
            clap::builder::ArgPredicate::Equals("true".into()),
            Some("true")
        )
    )]
    allow_empty: bool,
}

#[derive(clap::Args, Debug)]
struct FetchArgs {
    /// ibundle file to fetch
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,

    /// Perform a trial fetch without making changes to the repository
    #[arg(long)]
    dry_run: bool,

    /// Force fetch operation
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct ShowArgs {
    /// ibundle file to examine
    #[arg(value_name = "IBUNDLE_FILE")]
    ibundle_path: path::PathBuf,
}

#[derive(clap::Args, Debug)]
struct StatusArgs {}

#[derive(clap::Args, Debug)]
struct CleanArgs {
    /// Number of sequence numbers to retain
    #[arg(long,
        default_value = "20",
        value_parser = clap::value_parser!(u64).range(1..)
        )]
    keep: u64,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Create an ibundle
    Create(CreateArgs),

    /// Fetch from an ibundle
    Fetch(FetchArgs),

    /// Show details of an ibundle
    Show(ShowArgs),

    /// Report status
    Status(StatusArgs),

    /// Cleanup old sequence numbers
    Clean(CleanArgs),
}

//////////////////////////////////////////////////////////////////////////////

type RefName = BString;

// `name` => `Oid`.
type ORefs = collections::BTreeMap<RefName, git2::Oid>;
type ORefsItem<'a> = (&'a RefName, &'a git2::Oid);

trait CollectORefs {
    fn collect_orefs(self) -> ORefs;
}

impl<'a, T: IntoIterator<Item = ORefsItem<'a>>> CollectORefs for T {
    fn collect_orefs(self) -> ORefs {
        self.into_iter()
            .map(|(name, &oid)| (name.clone(), oid))
            .collect()
    }
}

fn orefs_write<'a, W: io::Write>(
    orefs: impl IntoIterator<Item = ORefsItem<'a>>,
    writer: &mut W,
) -> AResult<()> {
    for (name, oid) in orefs.into_iter() {
        write_oid_bstr_bline(writer, oid, name.as_bstr())?;
    }
    writer.write_all(b".\n")?;
    Ok(())
}

fn orefs_read<R: io::BufRead>(reader: &mut R) -> AResult<ORefs> {
    let mut orefs = ORefs::new();
    let mut bline = BString::from("");
    while read_bline(reader, &mut bline)? > 0 {
        if bline == "." {
            return Ok(orefs);
        }
        let (oid, name) = oid_bstr_parse(bline.as_bstr())?;
        orefs.insert(name, oid);
    }
    bail!("orefs: missing final '.'; got {}", quoted(bline));
}

// Each Oid is a "commit-ish" (an actual commit or a tag).
type Commits = collections::BTreeMap<git2::Oid, BString>;
type CommitsItem<'a> = (&'a git2::Oid, &'a BString);

fn commits_write<'a, W: io::Write>(
    commits: impl IntoIterator<Item = CommitsItem<'a>>,
    writer: &mut W,
) -> AResult<()> {
    for (oid, comment) in commits.into_iter() {
        write_oid_bstr_bline(writer, oid, comment.as_bstr())?;
    }
    writer.write_all(b".\n")?;
    Ok(())
}

fn commits_read<R: io::BufRead>(reader: &mut R) -> AResult<Commits> {
    let mut commits = Commits::new();
    let mut bline = BString::from("");
    while read_bline(reader, &mut bline)? > 0 {
        if bline == "." {
            return Ok(commits);
        }
        let (oid, comment) = oid_bstr_parse(bline.as_bstr())?;
        commits.insert(oid, comment);
    }
    bail!("commits: missing final '.'; got {}", quoted(bline));
}

fn repo_open<P: AsRef<std::path::Path>>(
    repo_path: P,
) -> AResult<git2::Repository> {
    let repo_path = repo_path.as_ref();
    let repo = match git2::Repository::open(repo_path) {
        Ok(repo) => repo,
        Err(_) => {
            bail!(
                "could not open Git repository at {}",
                quoted_path(repo_path)
            );
        }
    };
    Ok(repo)
}

fn repo_state_root_path(repo: &git2::Repository) -> path::PathBuf {
    repo.path().join("ibundle")
}

fn repo_temp_dir_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("temp")
}

fn repo_mktemp(repo: &git2::Repository) -> AResult<path::PathBuf> {
    let temp_dir_path = repo_temp_dir_path(repo);
    fs::create_dir_all(&temp_dir_path)?;
    Ok(temp_dir_path)
}

fn repo_meta_dir_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("repo_meta")
}

fn repo_meta_path(repo: &git2::Repository, seq_num: SeqNum) -> path::PathBuf {
    repo_meta_dir_path(&repo).join(&seq_num.to_string())
}

fn repo_id_path(repo: &git2::Repository) -> path::PathBuf {
    repo_state_root_path(repo).join("id")
}

fn repo_orefs(repo: &git2::Repository) -> AResult<ORefs> {
    let mut orefs = ORefs::new();
    for r in repo.references()? {
        let r = r?;
        let oid = if let Some(oid) = r.target() {
            oid
        } else {
            bail!("found non-direct ref kind {:?}", r.kind());
        };
        let name = RefName::from(r.name_bytes());
        orefs.insert(name, oid);
    }
    Ok(orefs)
}

fn repo_is_empty(repo: &git2::Repository) -> AResult<bool> {
    let orefs = repo_orefs(repo)?;
    Ok(orefs.len() == 0)
}

fn repo_find_missing_commits<'a>(
    repo: &git2::Repository,
    commits: impl IntoIterator<Item = CommitsItem<'a>>,
) -> Commits {
    commits
        .into_iter()
        .filter_map(|(&commit_id, comment)| {
            if !repo.find_commit(commit_id).is_ok() {
                Some((commit_id, comment.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn repo_remove_refs(
    repo: &git2::Repository,
    refs_to_remove: &collections::HashSet<BString>,
) -> AResult<()> {
    for res in repo.references()? {
        let mut r = res?;
        if refs_to_remove.contains(r.name_bytes().as_bstr()) {
            r.delete()?;
        }
    }
    Ok(())
}

//////////////////////////////////////////////////////////////////////////////

struct Directive {}
impl Directive {
    const REPO_ID: &[u8] = b"repo_id";
    const SEQ_NUM: &[u8] = b"seq_num";
    const BASIS_SEQ_NUM: &[u8] = b"basis_seq_num";
    const HEAD_REF: &[u8] = b"head_ref";
    const HEAD_DETACHED: &[u8] = b"head_detached";
    const OREFS: &[u8] = b"orefs";
    const COMMITS: &[u8] = b"commits";
    const PREREQS: &[u8] = b"prereqs";
    const ADDED_PACKED_OREFS: &[u8] = b"added_packed_orefs";
    const ADDED_NOT_PACKED_OREFS: &[u8] = b"added_not_packed_orefs";
    const REMOVED_OREFS: &[u8] = b"removed_orefs";
    const MOVED_PACKED_OREFS: &[u8] = b"moved_packed_orefs";
    const MOVED_NOT_PACKED_OREFS: &[u8] = b"moved_not_packed_orefs";
    const UNCHANGED_OREFS: &[u8] = b"unchanged_orefs";
}

fn write_directive<W: io::Write, D: AsRef<[u8]>, Rest: AsRef<[u8]>>(
    writer: &mut W,
    directive: D,
    rest: Rest,
) -> AResult<()> {
    writer.write_all(b"%")?;
    writer.write_all(directive.as_ref())?;
    writer.write_all(b" ")?;
    writer.write_all(rest.as_ref())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_directive_bool<W: io::Write, D: AsRef<[u8]>>(
    writer: &mut W,
    directive: D,
    value: bool,
) -> AResult<()> {
    let value_bstr = bool_as_bstr(value);
    writer.write_all(b"%")?;
    writer.write_all(directive.as_ref())?;
    writer.write_all(b" ")?;
    writer.write_all(value_bstr)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoMeta {
    head_ref: BString,
    head_detached: bool,
    orefs: ORefs,
    commits: Commits,
}

impl RepoMeta {
    fn new() -> Self {
        Self {
            head_ref: BString::from(""),
            head_detached: false,
            orefs: ORefs::new(),
            commits: Commits::new(),
        }
    }

    fn read<R: io::BufRead>(reader: &mut R) -> AResult<Self> {
        let mut bline = BString::from("");
        read_bline(reader, &mut bline)?;
        if bline != REPO_META_FORMAT_V1 {
            bail!("invalid repo meta file");
        }

        let mut meta = Self::new();
        while read_bline(reader, &mut bline)? > 0 {
            if bline.starts_with(b"%") {
                let (dir, rest) = bstr_pop_word(bline[1..].as_bstr());
                if dir == Directive::HEAD_REF {
                    meta.head_ref = BString::from(rest);
                } else if dir == Directive::HEAD_DETACHED {
                    meta.head_detached = parse_bool(rest.as_bstr())?;
                } else if dir == Directive::COMMITS {
                    meta.commits = commits_read(reader)?;
                } else if dir == Directive::OREFS {
                    meta.orefs = orefs_read(reader)?;
                } else {
                    bail!("invalid RepoMeta directive {}", bline);
                }
            } else {
                bail!("invalid RepoMeta line {}", bline);
            }
        }
        Ok(meta)
    }

    fn write<W: io::Write>(&self, writer: &mut W) -> AResult<()> {
        writer.write_all(REPO_META_FORMAT_V1)?;
        writer.write_all(b"\n")?;
        write_directive(writer, Directive::HEAD_REF, &self.head_ref)?;
        write_directive_bool(
            writer,
            Directive::HEAD_DETACHED,
            self.head_detached,
        )?;
        write_directive(writer, Directive::COMMITS, "")?;
        commits_write(&self.commits, writer)?;
        write_directive(writer, Directive::OREFS, "")?;
        orefs_write(&self.orefs, writer)?;
        writer.write_all(b"\n")?;
        Ok(())
    }
}

fn git_bundle_header_read<R: io::BufRead>(
    reader: &mut R,
) -> AResult<(Commits, ORefs)> {
    let mut bline = BString::from("");
    let mut prereqs = Commits::new();
    let mut orefs = ORefs::new();
    read_bline(reader, &mut bline)?;

    if bline != GIT_BUNDLE_FORMAT_V2 {
        bail!("not a V2 bundle file");
    }
    while read_bline(reader, &mut bline)? > 0 {
        if bline[0] == b'-' {
            let (oid, comment) = oid_bstr_parse(bline[1..].as_bstr())?;
            prereqs.insert(oid, comment);
        } else {
            let (oid, name) = oid_bstr_parse(bline.as_bstr())?;
            orefs.insert(name, oid);
        }
    }
    Ok((prereqs, orefs))
}

fn git_bundle_header_write<'p, 'o, W: io::Write>(
    writer: &mut W,
    prereqs: impl IntoIterator<Item = CommitsItem<'p>>,
    orefs: impl IntoIterator<Item = ORefsItem<'o>>,
) -> AResult<()> {
    writer.write_all(GIT_BUNDLE_FORMAT_V2)?;
    writer.write_all(b"\n")?;
    for (commit_id, comment) in prereqs.into_iter() {
        writer.write_all(b"-")?;
        write_oid_bstr_bline(writer, commit_id, comment.as_bstr())?;
    }
    for (name, oid) in orefs.into_iter() {
        write_oid_bstr_bline(writer, oid, name.as_bstr())?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn handle_bundle_create_stderr<R: io::Read>(
    stderr: &mut io::BufReader<R>,
) -> io::Result<bool> {
    let mut bundle_empty = false;
    let mut bline = Vec::new();
    loop {
        read_bytes_until(stderr, &mut bline, |b| b == b'\n' || b == b'\r')?;
        if bline.len() == 0 {
            break;
        }
        if bline
            .as_bstr()
            .contains_str("Refusing to create empty bundle")
        {
            bundle_empty = true;
        } else {
            io::stderr().write_all(&bline)?;
        }
    }
    Ok(bundle_empty)
}

fn git_bundle_create_stdin(
    bundle_path: &path::Path,
    stdin: fs::File,
) -> AResult<()> {
    let mut args: Vec<ffi::OsString> = vec!["bundle".into(), "create".into()];
    if !log_enabled!(Level::Info) {
        args.push("-q".into());
    }
    args.push(bundle_path.as_os_str().into());
    args.push("--stdin".into());

    let mut child = std::process::Command::new("git")
        .args(args)
        .stdin(stdin)
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stderr = io::BufReader::new(
        child
            .stderr
            .take()
            .expect("Command failed to provide `stderr`"),
    );

    let bundle_empty = handle_bundle_create_stderr(&mut stderr)?;
    let exit_status = child.wait()?;

    if bundle_empty {
        // `empty_pack_bytes` comes from:
        //   `git pack-objects --stdout < /dev/null > empty.pack`
        let empty_pack_bytes = vec![
            0x50, 0x41, 0x43, 0x4b, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
            0x00, 0x02, 0x9d, 0x08, 0x82, 0x3b, 0xd8, 0xa8, 0xea, 0xb5, 0x10,
            0xad, 0x6a, 0xc7, 0x5c, 0x82, 0x3c, 0xfd, 0x3e, 0xd3, 0x1e,
        ];

        let mut writer = create_writer(&bundle_path)?;
        git_bundle_header_write(&mut writer, &Commits::new(), &ORefs::new())?;
        writer.write_all(&empty_pack_bytes)?;
    } else if !exit_status.success() {
        bail!("failure in git bundle create");
    }
    Ok(())
}

fn git_fetch_bundle(bundle_path: &path::Path, dry_run: bool) -> AResult<()> {
    let mut args: Vec<ffi::OsString> = vec!["fetch".into(), "--force".into()];
    if !log_enabled!(Level::Info) {
        args.push("-q".into())
    }
    if dry_run {
        args.push("--dry-run".into())
    }
    args.push(bundle_path.as_os_str().into());
    args.push("*:*".into());
    let mut child = std::process::Command::new("git").args(args).spawn()?;
    let exit_status = child.wait()?;
    if !exit_status.success() {
        bail!("failure in git fetch");
    }
    Ok(())
}

//////////////////////////////////////////////////////////////////////////////

fn repo_fetch(
    repo: &git2::Repository,
    prereqs: &Commits,
    bundle_orefs: &ORefs,
    mut pack_reader: impl io::Read,
    dry_run: bool,
) -> AResult<()> {
    let temp_dir_path = repo_mktemp(repo)?;
    let bundle_path = temp_dir_path.join("temp.bundle");
    let bundle_path_deleter = FileDeleter::new(&bundle_path);
    let mut bundle_file = fs::File::create(&bundle_path)?;

    git_bundle_header_write(&mut bundle_file, prereqs, bundle_orefs)?;
    io::copy(&mut pack_reader, &mut bundle_file)?;
    drop(pack_reader);
    bundle_file.flush()?;
    drop(bundle_file);

    git_fetch_bundle(&bundle_path, dry_run)?;
    drop(bundle_path_deleter);

    Ok(())
}

// git2::Repository::set_head() is below:
//
//   pub fn set_head(&self, refname: &str) -> Result<(), Error> {
//       let refname = CString::new(refname)?;
//       unsafe {
//           try_call!(raw::git_repository_set_head(self.raw, refname));
//       }
//       Ok(())
//   }
//
// The passed-in `&str` value `refname` is not interpreted in any way; it's
// simply passed along into `git_repository_set_head()`, a function that expects
// raw bytes and does not require UTF8 semantics.
//
// Until git2 provides a `set_head_bytes()` function as requested in
// <https://github.com/rust-lang/git2-rs/issues/925>, and provided in pull
// request <https://github.com/rust-lang/git2-rs/pull/931>, the only way to
// support non-utf8 head values with git2 is to use the unsafe conversion
// `String::from_utf8_unchecked()`.

fn repo_set_head_ref(
    repo: &git2::Repository,
    head_ref: impl AsRef<BStr>,
) -> AResult<()> {
    let head_ref = head_ref.as_ref();
    if let Ok(head_ref_str) = name_to_string(head_ref) {
        repo.set_head(&head_ref_str)?;
    } else {
        // `head_ref` is non-utf8.
        let head_ref_bytes = head_ref.to_vec();
        // Safety: repo.set_head() does not interpret its argument as a
        // utf8-string; it merely passed it along to the underlying library
        // that's expecting raw bytes.
        repo.set_head(&unsafe { String::from_utf8_unchecked(head_ref_bytes) })?;
    }
    Ok(())
}

fn repo_has_oid(repo: &git2::Repository, oid: git2::Oid) -> bool {
    repo.find_object(oid, None).is_ok()
}

fn repo_commit(
    repo: &git2::Repository,
    commit_id: git2::Oid,
) -> AResult<git2::Commit> {
    Ok(repo.find_object(commit_id, None)?.peel_to_commit()?)
}

fn commit_comment(commit: &git2::Commit) -> BString {
    let comment = if let Some(summary) = commit.summary_bytes() {
        BString::from(summary)
    } else {
        BString::from("")
    };
    comment
}

fn repo_commit_id_comment(
    repo: &git2::Repository,
    commit_id: git2::Oid,
) -> AResult<(git2::Oid, BString)> {
    let commit = repo_commit(repo, commit_id)?;
    Ok((commit.id(), commit_comment(&commit)))
}

fn repo_seq_nums(repo: &git2::Repository) -> AResult<SeqNums> {
    let mut seq_nums = SeqNums::new();
    let meta_dir_path = repo_meta_dir_path(&repo);
    if let Ok(dir_iter) = fs::read_dir(&meta_dir_path) {
        for entry in dir_iter {
            if let Ok(seq_num) =
                entry?.file_name().to_string_lossy().parse::<u64>()
            {
                seq_nums.push(seq_num);
            }
        }
    }
    seq_nums.sort_by(|a, b| b.cmp(a));
    Ok(seq_nums)
}

fn repo_has_basis(repo: &git2::Repository, basis_seq_num: &SeqNum) -> bool {
    if let Ok(seq_nums) = repo_seq_nums(&repo) {
        seq_nums.contains(basis_seq_num)
    } else {
        false
    }
}

fn repo_id_new() -> BString {
    BString::from(uuid::Uuid::new_v4().to_string())
}

fn repo_id_read(repo: &git2::Repository) -> Option<BString> {
    fs::read_to_string(&repo_id_path(repo))
        .ok()
        .map(|s| BString::from(s.trim_end()))
}

fn repo_id_write(repo: &git2::Repository, repo_id: &BStr) -> AResult<()> {
    fs::create_dir_all(&repo_state_root_path(repo))?;
    let mut id_bytes = BString::from(repo_id);
    id_bytes.push(b'\n');
    fs::write(&repo_id_path(repo), id_bytes)?;
    Ok(())
}

fn repo_meta_current(repo: &git2::Repository) -> AResult<RepoMeta> {
    let mut meta = RepoMeta::new();
    meta.orefs = repo_orefs(&repo)?;
    let head_ref = repo
        .find_reference("HEAD")
        .context("cannot find `HEAD` reference")?;
    meta.head_detached = repo
        .head_detached()
        .context("failed to determine detached state")?;
    if meta.head_detached {
        let head_commit_id = head_ref.target().ok_or(anyhow!(
            "cannot retrieve detached-head commit id for `HEAD`"
        ))?;
        meta.head_ref = oid_to_bstring(&head_commit_id);
    } else {
        let head_sym_target = head_ref
            .symbolic_target_bytes()
            .ok_or(anyhow!("cannot retrieve symbolic ref for `HEAD`"))?;
        meta.head_ref = BString::from(head_sym_target);
    }
    if let Ok(head_commit) = head_ref.peel_to_commit() {
        meta.orefs.insert(BString::from("HEAD"), head_commit.id());
    }
    for (_name, &oid) in meta.orefs.iter() {
        let (commit_id, comment) = repo_commit_id_comment(repo, oid)?;
        meta.commits.insert(commit_id, comment);
    }
    Ok(meta)
}

fn repo_meta_read(
    repo: &git2::Repository,
    seq_num: SeqNum,
) -> AResult<RepoMeta> {
    let meta_path = repo_meta_path(repo, seq_num);
    let mut f = open_reader(&meta_path)?;
    RepoMeta::read(&mut f)
}

fn repo_meta_write(
    repo: &git2::Repository,
    seq_num: SeqNum,
    meta: &RepoMeta,
) -> AResult<()> {
    let meta_dir_path = repo_meta_dir_path(&repo);
    fs::create_dir_all(&meta_dir_path)?;
    let meta_path = repo_meta_path(repo, seq_num);
    let mut f = create_writer(&meta_path)?;
    meta.write(&mut f)
}

//////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone)]
struct IBundle {
    repo_id: BString,
    seq_num: SeqNum,
    basis_seq_num: SeqNum,
    head_ref: BString,
    head_detached: bool,
    prereqs: Commits,
    added_orefs: ORefs,
    removed_orefs: ORefs,
    moved_orefs: ORefs,
    unchanged_orefs: Option<ORefs>,
    packed_orefs: ORefs,
}

impl IBundle {
    fn construct(
        repo_id: BString,
        seq_num: SeqNum,
        basis_seq_num: SeqNum,
        meta: &RepoMeta,
        basis_meta: &RepoMeta,
    ) -> AResult<Self> {
        let mut removed_orefs = ORefs::new();
        for (name, &oid) in basis_meta.orefs.iter() {
            if !meta.orefs.contains_key(name) {
                removed_orefs.insert(name.clone(), oid);
            }
        }

        let mut added_orefs = ORefs::new();
        let mut moved_orefs = ORefs::new();
        let mut unchanged_orefs = ORefs::new();
        for (name, &oid) in meta.orefs.iter() {
            let name = name.clone();
            if let Some(&oid2) = basis_meta.orefs.get(&name) {
                if oid == oid2 {
                    unchanged_orefs.insert(name, oid);
                } else {
                    moved_orefs.insert(name, oid);
                }
            } else {
                added_orefs.insert(name, oid);
            }
        }

        let ibundle = IBundle {
            repo_id,
            seq_num,
            basis_seq_num,
            head_ref: meta.head_ref.clone(),
            head_detached: meta.head_detached,
            prereqs: Commits::new(),
            added_orefs: added_orefs,
            removed_orefs: removed_orefs,
            moved_orefs,
            unchanged_orefs: Some(unchanged_orefs),
            packed_orefs: ORefs::new(),
        };

        Ok(ibundle)
    }

    fn new() -> Self {
        Self {
            repo_id: BString::from(""),
            seq_num: 0,
            basis_seq_num: 0,
            head_ref: BString::from(""),
            head_detached: false,
            prereqs: Commits::new(),
            added_orefs: ORefs::new(),
            removed_orefs: ORefs::new(),
            moved_orefs: ORefs::new(),
            unchanged_orefs: None,
            packed_orefs: ORefs::new(),
        }
    }

    fn read<R: io::BufRead>(reader: &mut R) -> AResult<Self> {
        let mut bline = BString::from("");
        read_bline(reader, &mut bline)?;
        if bline != IBUNDLE_FORMAT_V2 {
            bail!("not a V2 ibundle file");
        }

        let mut ibundle = Self::new();
        let mut added_packed_orefs = ORefs::new();
        let mut added_not_packed_orefs = ORefs::new();
        let mut moved_packed_orefs = ORefs::new();
        let mut moved_not_packed_orefs = ORefs::new();
        while read_bline(reader, &mut bline)? > 0 {
            if bline.starts_with(b"%") {
                let (dir, rest) = bstr_pop_word(bline[1..].as_bstr());
                if dir == Directive::REPO_ID {
                    ibundle.repo_id = BString::from(rest);
                } else if dir == Directive::SEQ_NUM {
                    ibundle.seq_num = parse_seq_num(rest)?;
                } else if dir == Directive::BASIS_SEQ_NUM {
                    ibundle.basis_seq_num = parse_seq_num(rest)?;
                } else if dir == Directive::HEAD_REF {
                    ibundle.head_ref = BString::from(rest);
                } else if dir == Directive::HEAD_DETACHED {
                    ibundle.head_detached = parse_bool(rest.as_bstr())?;
                } else if dir == Directive::PREREQS {
                    ibundle.prereqs = commits_read(reader)?;
                } else if dir == Directive::ADDED_PACKED_OREFS {
                    added_packed_orefs = orefs_read(reader)?;
                } else if dir == Directive::ADDED_NOT_PACKED_OREFS {
                    added_not_packed_orefs = orefs_read(reader)?;
                } else if dir == Directive::REMOVED_OREFS {
                    ibundle.removed_orefs = orefs_read(reader)?;
                } else if dir == Directive::MOVED_PACKED_OREFS {
                    moved_packed_orefs = orefs_read(reader)?;
                } else if dir == Directive::MOVED_NOT_PACKED_OREFS {
                    moved_not_packed_orefs = orefs_read(reader)?;
                } else if dir == Directive::UNCHANGED_OREFS {
                    ibundle.unchanged_orefs = Some(orefs_read(reader)?);
                } else {
                    bail!("invalid ibundle directive {}", bline);
                }
            } else {
                bail!("invalid ibundle line {}", bline);
            }
        }
        ibundle.added_orefs = added_packed_orefs
            .iter()
            .chain(added_not_packed_orefs.iter())
            .collect_orefs();
        ibundle.moved_orefs = moved_packed_orefs
            .iter()
            .chain(moved_not_packed_orefs.iter())
            .collect_orefs();
        ibundle.packed_orefs = added_packed_orefs
            .into_iter()
            .chain(moved_packed_orefs.into_iter())
            .collect();

        Ok(ibundle)
    }

    fn write<W: io::Write>(
        &self,
        writer: &mut W,
        standalone: bool,
    ) -> AResult<()> {
        let unchanged_orefs = if let Some(unchanged) = &self.unchanged_orefs {
            unchanged
        } else {
            bail!("trying to write ibundle without unchanged_refs");
        };
        writer.write_all(IBUNDLE_FORMAT_V2)?;
        writer.write_all(b"\n")?;
        write_directive(writer, Directive::REPO_ID, &self.repo_id)?;
        write_directive(
            writer,
            Directive::SEQ_NUM,
            &format!("{}", self.seq_num),
        )?;
        write_directive(
            writer,
            Directive::BASIS_SEQ_NUM,
            &format!("{}", self.basis_seq_num),
        )?;
        write_directive(writer, Directive::HEAD_REF, &self.head_ref)?;
        write_directive_bool(
            writer,
            Directive::HEAD_DETACHED,
            self.head_detached,
        )?;
        write_directive(writer, Directive::PREREQS, "")?;
        commits_write(&self.prereqs, writer)?;
        write_directive(writer, Directive::ADDED_PACKED_OREFS, "")?;
        orefs_write(
            self.added_orefs
                .iter()
                .filter(|(name, _oid)| self.packed_orefs.contains_key(*name)),
            writer,
        )?;
        write_directive(writer, Directive::ADDED_NOT_PACKED_OREFS, "")?;
        orefs_write(
            self.added_orefs
                .iter()
                .filter(|(name, _oid)| !self.packed_orefs.contains_key(*name)),
            writer,
        )?;
        write_directive(writer, Directive::REMOVED_OREFS, "")?;
        orefs_write(&self.removed_orefs, writer)?;
        write_directive(writer, Directive::MOVED_PACKED_OREFS, "")?;
        orefs_write(
            self.moved_orefs
                .iter()
                .filter(|(name, _oid)| self.packed_orefs.contains_key(*name)),
            writer,
        )?;
        write_directive(writer, Directive::MOVED_NOT_PACKED_OREFS, "")?;
        orefs_write(
            self.moved_orefs
                .iter()
                .filter(|(name, _oid)| !self.packed_orefs.contains_key(*name)),
            writer,
        )?;
        if standalone {
            write_directive(writer, Directive::UNCHANGED_OREFS, "")?;
            orefs_write(unchanged_orefs, writer)?;
        }
        writer.write_all(b"\n")?;
        Ok(())
    }

    fn validate_repo_identity(
        &self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<()> {
        if let Some(repo_id) = repo_id_read(&repo) {
            if repo_id != self.repo_id {
                bail!(
                    "repo's repo_id({}) != ibundle repo_id({})",
                    repo_id,
                    self.repo_id
                );
            }
        } else if !force && !repo_is_empty(repo)? {
            bail!("repo lacks repo_id and is non-empty; consider `--force`");
        }

        Ok(())
    }

    fn determine_basis_meta(
        &self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<RepoMeta> {
        let basis_meta = if self.basis_seq_num == 0 {
            RepoMeta::new()
        } else if repo_has_basis(repo, &self.basis_seq_num) {
            repo_meta_read(&repo, self.basis_seq_num)?
        } else if self.unchanged_orefs.is_none() {
            bail!(
                std::concat!(
                    "repo missing basis_seq_num={} and ibundle is not ",
                    "standalone; consider `create --standalone`"
                ),
                self.basis_seq_num
            );
        } else if !force {
            bail!(
                std::concat!(
                    "repo missing basis_seq_num={}, but ibundle is ",
                    "standalone; consider `--force`",
                ),
                self.basis_seq_num
            );
        } else {
            RepoMeta::new()
        };

        Ok(basis_meta)
    }

    fn apply_basis_meta(&mut self, basis_meta: &RepoMeta) -> AResult<()> {
        if self.unchanged_orefs.is_none() {
            let mut unchanged_orefs = ORefs::new();
            for (name, &oid) in basis_meta.orefs.iter() {
                if !self.removed_orefs.contains_key(name)
                    && !self.moved_orefs.contains_key(name)
                {
                    unchanged_orefs.insert(name.clone(), oid);
                }
            }
            self.unchanged_orefs = Some(unchanged_orefs);
        }
        Ok(())
    }

    fn validate_and_apply_basis(
        &mut self,
        repo: &git2::Repository,
        force: bool,
    ) -> AResult<()> {
        self.validate_repo_identity(repo, force)?;
        let basis_meta = self.determine_basis_meta(&repo, force)?;
        self.apply_basis_meta(&basis_meta)?;
        Ok(())
    }

    fn delta_orefs(&self) -> AResult<ORefs> {
        Ok(self
            .added_orefs
            .iter()
            .chain(self.moved_orefs.iter())
            .collect_orefs())
    }

    fn full_orefs(&self) -> AResult<ORefs> {
        let mut orefs = self.delta_orefs()?;
        if let Some(unchanged_orefs) = &self.unchanged_orefs {
            orefs.extend(unchanged_orefs.clone().into_iter());
        } else {
            bail!("using full_refs() on ibundle without `unchanged_orefs`");
        }
        Ok(orefs)
    }

    fn summary(&self) -> String {
        format!(
            "seq_num {}, added {}, removed {}, moved {}, unchanged {}",
            self.seq_num,
            self.added_orefs.len(),
            self.removed_orefs.len(),
            self.moved_orefs.len(),
            if let Some(unchanged_orefs) = &self.unchanged_orefs {
                format!("{}", unchanged_orefs.len())
            } else {
                format!("???")
            }
        )
    }
}

//////////////////////////////////////////////////////////////////////////////

fn inc_seq_num(seq_num: &SeqNum) -> AResult<SeqNum> {
    let next_seq_num = match seq_num.checked_add(1) {
        Some(next_seq_num) => next_seq_num,
        None => bail!("seq_num({}) too large", seq_num),
    };
    Ok(next_seq_num)
}

fn calc_max_seq_num(seq_nums: &SeqNums) -> AResult<SeqNum> {
    let max_seq_num = if seq_nums.len() > 0 { seq_nums[0] } else { 0 };
    Ok(max_seq_num)
}

fn calc_next_seq_num(seq_nums: &SeqNums) -> AResult<SeqNum> {
    inc_seq_num(&calc_max_seq_num(seq_nums)?)
}

fn calc_basis_seq_num(
    basis_option: Option<SeqNum>,
    seq_nums: &SeqNums,
    cur_seq_num: SeqNum,
) -> AResult<SeqNum> {
    let basis_seq_num = basis_option.unwrap_or(cur_seq_num - 1);
    if basis_seq_num > 0 && !seq_nums.contains(&basis_seq_num) {
        bail!("basis not present for `--basis {}`", basis_seq_num);
    }
    Ok(basis_seq_num)
}

//////////////////////////////////////////////////////////////////////////////

fn read_ibundle<P: AsRef<std::path::Path>>(
    ibundle_path: P,
) -> AResult<(IBundle, io::BufReader<fs::File>)> {
    let ibundle_path = ibundle_path.as_ref();
    let mut ibundle_reader = open_reader(ibundle_path)?;
    let ibundle = IBundle::read(&mut ibundle_reader).with_context(|| {
        format!("failure reading ibundle file {}", quoted_path(ibundle_path))
    })?;

    Ok((ibundle, ibundle_reader))
}

fn cmd_create(create_args: &CreateArgs) -> AResult<i32> {
    let repo_path = ".";
    let repo = repo_open(repo_path)?;
    let repo_id = if let Some(repo_id) = repo_id_read(&repo) {
        repo_id
    } else {
        let repo_id = repo_id_new();
        repo_id_write(&repo, repo_id.as_bstr())?;
        repo_id
    };

    let seq_nums = repo_seq_nums(&repo)?;
    let seq_num = calc_next_seq_num(&seq_nums)?;
    let meta = repo_meta_current(&repo)?;

    let basis_seq_num;
    let basis_meta;
    if create_args.basis_current {
        basis_seq_num = seq_num;
        basis_meta = meta.clone();
    } else {
        basis_seq_num =
            calc_basis_seq_num(create_args.basis, &seq_nums, seq_num)?;
        basis_meta = if basis_seq_num > 0 {
            repo_meta_read(&repo, basis_seq_num)?
        } else {
            RepoMeta::new()
        };
    }

    let mut ibundle = IBundle::construct(
        repo_id,
        seq_num,
        basis_seq_num,
        &meta,
        &basis_meta,
    )?;

    if meta == basis_meta && !create_args.allow_empty {
        if log_enabled!(Level::Error) {
            eprintln!(std::concat!(
                "error: refusing to create an empty ibundle; ",
                "consider `--allow-empty`"
            ));
        }
        return Ok(STATUS_EMPTY_BUNDLE);
    }

    // OIDs for still-valid commits and refs are fair game to exclude.
    let excluded_oids = basis_meta
        .commits
        .keys()
        .chain(basis_meta.orefs.values())
        .filter(|oid| repo_has_oid(&repo, **oid))
        .collect::<collections::HashSet<_>>();

    let bundle_orefs = if create_args.standalone {
        ibundle.full_orefs()?
    } else {
        ibundle.delta_orefs()?
    };

    let temp_dir_path = repo_mktemp(&repo)?;
    let bundle_path = temp_dir_path.join("temp.bundle");
    let bundle_path_deleter = FileDeleter::new(&bundle_path);

    let stdin_path = temp_dir_path.join("temp.stdin");
    let stdin_path_deleter = FileDeleter::new(&stdin_path);

    let mut stdin_file = fs::File::create(&stdin_path)?;
    for oid in excluded_oids.iter() {
        stdin_file.write_all(b"^")?;
        stdin_file.write_all(oid_to_bstring(oid).as_bstr())?;
        stdin_file.write_all(b"\n")?;
    }
    for (name, _oid) in bundle_orefs.iter() {
        write_bline(&mut stdin_file, name.as_bstr())?;
    }
    stdin_file.flush()?;
    drop(stdin_file);

    git_bundle_create_stdin(&bundle_path, open_file(&stdin_path)?)?;
    drop(stdin_path_deleter);

    let mut bundle_reader = open_reader(&bundle_path)?;
    let (mut prereqs, packed_orefs) =
        git_bundle_header_read(&mut bundle_reader)?;

    for (name, &oid) in bundle_orefs.iter() {
        if !packed_orefs.contains_key(name) {
            // Git thinks we don't need this `oref` because the associated
            // object (tag or commit) was excluded by the basis.  We want it
            // anyway, so add the associated commit to the `prereqs`.
            if let Ok(obj) = repo.find_object(oid, None) {
                if let Ok(commit) = obj.peel_to_commit() {
                    let commit_id = commit.id();
                    if !prereqs.contains_key(&commit_id) {
                        prereqs.insert(commit_id, commit_comment(&commit));
                    }
                }
            }
        }
    }

    ibundle.prereqs = prereqs;
    ibundle.packed_orefs = packed_orefs;

    let mut ibundle_writer = create_writer(&create_args.ibundle_path)?;
    ibundle.write(&mut ibundle_writer, create_args.standalone)?;
    io::copy(&mut bundle_reader, &mut ibundle_writer)?;
    drop(bundle_reader);
    drop(bundle_path_deleter);
    ibundle_writer.flush()?;
    drop(ibundle_writer);

    repo_meta_write(&repo, seq_num, &meta)?;
    log::info!(
        "wrote {}: {}",
        quoted_path(&create_args.ibundle_path),
        ibundle.summary()
    );
    Ok(STATUS_OK)
}

fn cmd_fetch(fetch_args: &FetchArgs) -> AResult<i32> {
    if fetch_args.dry_run {
        log::info!("(dry run)");
    }

    let repo_path = ".";
    let repo = repo_open(repo_path)?;

    if !repo.is_bare() {
        bail!("cannot fetch into non-bare repository");
    }

    let ibundle_path = &fetch_args.ibundle_path;
    let (mut ibundle, ibundle_reader) = read_ibundle(ibundle_path)?;

    ibundle.validate_and_apply_basis(&repo, fetch_args.force)?;

    log::info!("read {}: {}", quoted_path(&ibundle_path), ibundle.summary());

    let mut ready_for_ibundle = true;

    let missing_prereqs = repo_find_missing_commits(&repo, &ibundle.prereqs);
    if missing_prereqs.len() > 0 {
        ready_for_ibundle = false;
        if log_enabled!(Level::Error) {
            eprintln!(
                "repo is missing {} prerequisites listed in ibundle",
                missing_prereqs.len()
            );
            if log_enabled!(Level::Debug) {
                for (oid, msg) in missing_prereqs.iter() {
                    eprintln!("  {:?} {}", oid, quoted(msg));
                }
            } else {
                eprintln!(" (use `--verbose` to display them)");
            }
        }
    }

    let full_orefs = ibundle.full_orefs()?;

    // OIDs not being created by the pack must pre-exist.
    let missing_orefs = full_orefs
        .iter()
        .filter(|(name, oid)| {
            !ibundle.packed_orefs.contains_key(*name)
                && !repo_has_oid(&repo, **oid)
        })
        .collect_orefs();
    if missing_orefs.len() > 0 {
        ready_for_ibundle = false;
        if log_enabled!(Level::Error) {
            eprintln!(
                "repo is missing {} orefs for basis_seq_num {}",
                missing_orefs.len(),
                ibundle.basis_seq_num
            );
            if log_enabled!(Level::Debug) {
                for (name, oid) in missing_orefs.iter() {
                    eprintln!("  {:?} {}", oid, quoted(name));
                }
            } else {
                eprintln!(" (use `--verbose` to display them)");
            }
        }
    }

    if !ready_for_ibundle {
        bail!(
            "repo not ready for ibundle with basis_seq_num {}",
            ibundle.basis_seq_num
        );
    }

    if !fetch_args.dry_run {
        repo_id_write(&repo, ibundle.repo_id.as_bstr())?;
    }

    let pre_meta = repo_meta_current(&repo)?;
    let mut refs_to_remove = pre_meta
        .orefs
        .iter()
        .filter_map(|(name, _oid)| {
            if name != b"HEAD".as_bstr() && !full_orefs.contains_key(name) {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect::<collections::HashSet<_>>();

    let mut bundle_orefs = full_orefs
        .iter()
        .filter(|(name, oid)| {
            *name != b"HEAD".as_bstr() && pre_meta.orefs.get(*name) != Some(oid)
        })
        .collect_orefs();

    if let Some(&head_oid) = ibundle.packed_orefs.get(b"HEAD".as_bstr()) {
        let packed_oids = ibundle
            .packed_orefs
            .iter()
            .filter_map(|(name, &oid)| {
                if name != b"HEAD".as_bstr() {
                    Some(oid)
                } else {
                    None
                }
            })
            .collect::<collections::HashSet<_>>();
        if !packed_oids.contains(&head_oid) {
            let mut h = BString::from("refs/heads/HEAD-");
            h.push_str(oid_to_bstring(&head_oid));
            bundle_orefs.insert(h.clone(), head_oid);
            refs_to_remove.insert(h);
        }
    }

    repo_fetch(
        &repo,
        &ibundle.prereqs,
        &bundle_orefs,
        ibundle_reader,
        fetch_args.dry_run,
    )?;

    let head_ref = ibundle.head_ref.as_bstr();
    if !fetch_args.dry_run && head_ref != "" {
        if ibundle.head_detached {
            let commit_id = parse_oid(head_ref)?;
            repo.set_head_detached(commit_id)?;
        } else {
            repo_set_head_ref(&repo, head_ref)?;
        }
    }

    if !fetch_args.dry_run {
        repo_remove_refs(&repo, &refs_to_remove)?;
    }

    let post_meta = if fetch_args.dry_run {
        RepoMeta {
            head_ref: BString::from(head_ref),
            head_detached: ibundle.head_detached,
            orefs: full_orefs.clone(),
            commits: Commits::new(),
        }
    } else {
        repo_meta_current(&repo)?
    };

    if post_meta.orefs != full_orefs {
        bail!("final repository refs do not match those in ibundle");
    }
    if post_meta.head_ref != ibundle.head_ref
        || post_meta.head_detached != ibundle.head_detached
    {
        bail!(
            "repository HEAD ({}{}) does not match ibundle HEAD ({}{})",
            quoted(post_meta.head_ref),
            if post_meta.head_detached {
                ", detached"
            } else {
                ""
            },
            quoted(ibundle.head_ref),
            if ibundle.head_detached {
                ", detached"
            } else {
                ""
            },
        );
    }

    if !fetch_args.dry_run {
        repo_meta_write(&repo, ibundle.seq_num, &post_meta)?;
    }

    log::info!(
        "final state: {} refs, HEAD {}{}",
        post_meta.orefs.len(),
        quoted(&post_meta.head_ref),
        if post_meta.head_detached {
            " (detached)"
        } else {
            ""
        }
    );
    Ok(STATUS_OK)
}

fn yes_no(predicate: bool) -> String {
    format!("{}", if predicate { "yes" } else { "no" })
}

fn show_orefs(orefs: &ORefs) {
    if log_enabled!(Level::Debug) {
        for (name, oid) in orefs {
            log::debug!("{} {}", oid_to_bstring(oid).to_string(), quoted(name));
        }
        log::debug!(".");
    }
}

fn show_commits(commits: &Commits) {
    if log_enabled!(Level::Debug) {
        for (oid, comment) in commits {
            log::debug!(
                "{} {}",
                oid_to_bstring(oid).to_string(),
                quoted(comment)
            );
        }
        log::debug!(".");
    }
}

fn cmd_show(show_args: &ShowArgs) -> AResult<i32> {
    let ibundle_path = &show_args.ibundle_path;
    let (ibundle, ibundle_reader) = read_ibundle(ibundle_path)?;
    drop(ibundle_reader);
    log::info!("standalone: {}", yes_no(ibundle.unchanged_orefs.is_some()));
    log::info!("repo_id: {}", ibundle.repo_id);
    log::info!("seq_num: {}", ibundle.seq_num);
    log::info!("basis_seq_num: {}", ibundle.basis_seq_num);
    log::info!("head_ref: {}", quoted(&ibundle.head_ref));
    log::info!("head_detached: {}", yes_no(ibundle.head_detached));
    log::info!("added_orefs: {}", ibundle.added_orefs.len());
    show_orefs(&ibundle.added_orefs);
    log::info!("removed_orefs: {}", ibundle.removed_orefs.len());
    show_orefs(&ibundle.removed_orefs);
    log::info!("moved_orefs: {}", ibundle.moved_orefs.len());
    show_orefs(&ibundle.moved_orefs);
    if let Some(unchanged_orefs) = &ibundle.unchanged_orefs {
        log::info!("unchanged_orefs: {}", unchanged_orefs.len());
        show_orefs(&unchanged_orefs);
    }
    log::info!("prereqs: {}", ibundle.prereqs.len());
    show_commits(&ibundle.prereqs);
    Ok(STATUS_OK)
}

fn cmd_status(status_args: &StatusArgs) -> AResult<i32> {
    drop(status_args);
    let repo_path = ".";
    let repo = repo_open(repo_path)?;
    let mut failed = false;

    let repo_id = repo_id_read(&repo).unwrap_or(BString::from("NONE"));
    let seq_nums = repo_seq_nums(&repo)?;
    let max_seq_num = calc_max_seq_num(&seq_nums)?;
    let next_seq_num = calc_next_seq_num(&seq_nums)?;

    log::info!("repo_id: {}", repo_id);
    log::info!("max_seq_num: {}", max_seq_num);
    log::info!("next_seq_num: {}", next_seq_num);
    log::debug!("kept_seq_nums: {}", seq_nums.len());

    if log_enabled!(Level::Debug) {
        if seq_nums.len() > 0 {
            log::debug!("  {:<8} {:<8} {}", "seq_num", "num_refs", "HEAD");
            for &seq_num in seq_nums.iter().rev() {
                match repo_meta_read(&repo, seq_num) {
                    Ok(meta) => {
                        log::debug!(
                            "  {:<8} {:<8} {}{}",
                            seq_num,
                            meta.orefs.len(),
                            meta.head_ref,
                            if meta.head_detached {
                                " (detached)"
                            } else {
                                ""
                            }
                        );
                    }
                    Err(e) => {
                        log::debug!("  {:<8} **Error: {}", seq_num, e);
                        failed = true;
                    }
                }
            }
        }
    } else {
        log::info!("Use `--verbose` for details.");
    }

    Ok(if failed { STATUS_ERROR } else { STATUS_OK })
}

fn cmd_clean(clean_args: &CleanArgs) -> AResult<i32> {
    let repo_path = ".";
    let repo = repo_open(repo_path)?;

    if repo_id_read(&repo).is_none() {
        bail!("missing repo_id; no sequence numbers to clean");
    }
    let mut seq_nums = repo_seq_nums(&repo)?;
    let keep = usize::try_from(clean_args.keep).unwrap_or(usize::MAX);
    if seq_nums.len() <= keep {
        log::info!(
            "have {} sequence numbers, keeping up to {} => nothing to clean",
            seq_nums.len(),
            keep
        );
    } else {
        log::info!(
            "have {} sequence numbers, keeping up to {} => removing {}",
            seq_nums.len(),
            keep,
            seq_nums.len() - keep,
        );
        let meta_dir_path = repo_meta_dir_path(&repo);

        while seq_nums.len() > keep {
            if let Some(seq_num) = seq_nums.pop() {
                let meta_path = meta_dir_path.join(&seq_num.to_string());
                fs::remove_file(&meta_path).with_context(|| {
                    format!(
                        "failed to remove seq_num {} at {}",
                        seq_num,
                        quoted_path(&meta_path)
                    )
                })?;
            }
        }
    }

    Ok(STATUS_OK)
}

fn run() -> AResult<i32> {
    let cli = Cli::parse();
    env_logger::Builder::new()
        .filter_level(cli.verbose.log_level_filter())
        .format(|buf, record| writeln!(buf, "{}", record.args()))
        .target(env_logger::Target::Stdout)
        .init();
    let exit_status = match &cli.command {
        Commands::Create(create_args) => cmd_create(create_args)?,
        Commands::Fetch(fetch_args) => cmd_fetch(fetch_args)?,
        Commands::Show(show_args) => cmd_show(show_args)?,
        Commands::Status(status_args) => cmd_status(status_args)?,
        Commands::Clean(clean_args) => cmd_clean(clean_args)?,
    };
    Ok(exit_status)
}

fn main() {
    let exit_status = match run() {
        Ok(exit_status) => exit_status,
        Err(e) => {
            if log_enabled!(Level::Error) {
                eprintln!("error: {:?}", e);
            }
            STATUS_ERROR
        }
    };
    std::process::exit(exit_status);
}
