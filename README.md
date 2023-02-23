# git-ibundle

git-ibundle is a tool for incremental offline mirroring of a Git repository.

Incremental repository data is transferred from a source network to a
destination network via a sequence of "ibundle" (incremental bundle) files.  No
interactive connection is needed between the two networks; only a reliable
one-way file transfer capability is required.

## Typical transfer process

Consider mirroring `repo.git` from a source network to a disconnected
destination network.

First, perform one-time setup:

- On the source network, perform a mirror clone of the repository:

      git clone --mirror https://github.com/user/repo.git
      cd repo.git

- On the destination network, setup an empty bare repository to become the
  mirror:

      mkdir repo.git
      cd repo.git
      git init --bare

Next, repeat the following steps as often as desired to keep source
and destination repositories synchronized:

- On the source network, fetch any changes and create `repo.ibundle`:

      # On source network, within the `repo.git` directory:
      git fetch
      git-ibundle create .../path/to/repo.ibundle

- Transfer the `repo.ibundle` file to the destination network.

- On the destination network, fetch from `repo.ibundle`:

      # On destination network, within the `repo.git` directory:
      git-ibundle fetch .../path/to/repo.ibundle

## Change history

See [CHANGES](CHANGES.md) for a list of changes.

## License

git-ibundle is licensed under the terms of the MIT license; see
[LICENSE](LICENSE.md).

## Requirements

- `git-ibundle` executable
- Git v2.25+ (with `git` on the `PATH`)

Note: Git version 2.25 introduced the `git bundle create --progress` flag
required by git-ibundle.

Development and most testing is done on Linux; this is the best-supported
platform.  Limited testing is done on Windows.  No testing is done on Macos.

## Installation

Options for installation include:

- Downloading pre-built executables from the releases area for git-ibundle:
  <https://github.com/drmikehenry/git-ibundle/releases>

- Installing git-ibundle from `crates.io` via:

      cargo install git-ibundle

### Invocation as `git ibundle`

git-ibundle is named with a `git-` prefix so that it can integrate into Git as
the command `ibundle`.  If the executable `git-ibundle` is found on the
`PATH`, then the Git command `git ibundle` will delegate to `git-ibundle`.
These invocations are then equivalent:

    git-ibundle <ibundle-arguments>
    git ibundle <ibundle-arguments>

This allows git-ibundle to inherit some generic Git's functionality, the most
useful of which is:

    git -C path/to/repository <command>

This causes Git to to change the directory to `path/to/repository` before
running `<command>`.  For example:

    # Create directory and initialize as a bare Git repo:
    mkdir repo.git
    git -C repo.git init --bare

This is useful for `git-ibundle` as well.  Consider having a repository and an
ibundle file in the same directory:

    ./
        repo.git/
        repo.ibundle

To fetch from this ibundle into the repository, you could change into the
repository directory and fetch like this:

    cd repo.git
    git ibundle fetch ../repo.ibundle
    cd ..

Or you could use `-C repo.git` to do this in one step:

    git -C repo.git ibundle fetch ../repo.ibundle

Note that changing the directory occurs before `git-ibundle` examines its
arguments, so if you use a relative path to `repo.ibundle`, you must make that
path relative to the repository's location (which is why the example above uses
`../repo.ibundle`).

## Model

git-ibundle synchronizes two repositories at discrete synchronization points in
time.  Each time an ibundle is created via `git-ibundle create`, a new
synchronization point is defined, and the current repository state is recorded.
Repository state includes `HEAD` and all branches, tags, and associated commit
IDs.  An automatically incrementing sequence number provides a way to identify
the synchronization point and to label the associated ibundle file and current
repository state.

An ibundle file contains the source repository changes occurring between a
previous (basis) state and the current state.  At the destination, `git-ibundle
fetch` will apply these changes to the destination, synchronizing that
repository with the source.  git-ibundle verifies that the destination
repository has already applied the changes for the ibundle's basis.

By default, an ibundle is created using the immediately preceding sequence
number as a basis; it's possible to choose a different basis via `git-ibundle
create --basis <seq_num>`.  This is useful if any previous ibundle files have
been lost before fetching them into the destination repository.

For a repository `repo.git`, git-ibundle uses the directory `repo.git/ibundle/`
to hold its metadata.  This directory is transparent to Git and does not
interfere or overlap with normal Git operations.

## Mirroring a subset

git-ibundle itself always makes a complete mirror of the source repository.
This includes all references in the repository, including anything found below
`refs/remotes/<REMOTE>`.  The source repository should be cloned to a local
`repo.git` directory using `git clone --mirror` to prevent creation of
`refs/remotes/<REMOTE>` and ensure accurate mirroring.

It's possible to mirror a subset of the origin repository by setting up a
negative refspec.  For example, to avoid mirroring Github pull requests (which
have refspecs of the form `refs/pull/*`), the following negative refspec can be
used:

    remote.origin.fetch=^refs/pull/*

This can't be configured via `git clone --mirror --config` because the negative
refspec doesn't take effect soon enough; instead, manually setup the source
`repo.git` via:

    mkdir repo.git
    cd repo.git
    git init --bare
    git remote add origin --mirror=push https://github.com/user/repo.git
    git config remote.origin.fetch '+refs/*:refs/*'
    git config --add remote.origin.fetch '^refs/pull/*'

You may then fetch and verify that the refs are as expected:

    git fetch
    git show-ref

## Command invocation details

### Create an ibundle

```text
Usage: git-ibundle create [OPTIONS] <IBUNDLE_FILE>

Arguments:
  <IBUNDLE_FILE>  ibundle file to create

Options:
      --basis <BASIS>  Choose alternate basis sequence number
      --basis-current  Choose basis to be current repository state
      --standalone     Force ibundle to be standalone
      --allow-empty    Allow creation of an empty ibundle
  -h, --help           Print help information
  -V, --version        Print version information
  -v, --verbose...     More output per occurrence
  -q, --quiet...       Less output per occurrence
```

On the first ibundle creation, the repository is assigned a random repo_id.
This is used to help prevent accidental application of an ibundle file to the
wrong destination Git repository.  The repo_id will be checked during
`git-ibundle fetch` operations.

The basis sequence number defaults to one less than the ibundle's sequence
number; for the first ibundle (which will have sequence number `1`), the basis
sequence number will be `0`.

With `--basis 0`, the created ibundle will assume no prerequisite commits are
present at the destination; it will contain everything needed to create a mirror
repository via `git-ibundle fetch`.  Note that `--basis 0` implies
`--standalone`.

Without `--standalone`, the ibundle will be created with the assumption that the
destination has been synchronized to the `--basis` sequence number and thus
contains all prerequisite commits and references; as a result, the created
ibundle file contains only the changed references for compactness, along with a
Git "PACK" containing updated Git objects.

With `--standalone`, the ibundle will instead contain the full set of named
references and a full enumeration of prerequisite commit IDs.  Commit data will
still be incremental and based on the commits implied by `--basis`.  This may be
used for cases where the destination repository is known to have the
prerequisite commits but lacks the actual basis sequence number (e.g., when
using a pre-existing repository mirror on the destination network).

Normally, `git-ibundle create` will refuse to create an ibundle when there have
been no changes since the last ibundle was created.  In this case, an exit
status of `3` is provided (whereas most failures result in an exit status of
`1`).  To allow creation of an empty ibundle, use `--allow-empty`.

With `--basis-current` (which implies `--standalone` and `--allow-empty`), the
basis is set to the current repository state.  The ibundle will be logically
empty and standalone, making it suitable for fetching into an existing
destination repository known to match the state of the current repository.  This
provides a way to bootstrap into the use of git-ibundle for an existing pair of
mirrored repositories.  For example:

    # On source network:
    cd source.git
    git-ibundle create --basis-current ../bootstrap.ibundle

    # Transfer `bootstrap.ibundle` to destination network.

    # On destination network:
    cd destination.git
    git-ibundle fetch ../bootstrap.ibundle --force

### Fetch from an ibundle

```text
Usage: git-ibundle fetch [OPTIONS] <IBUNDLE_FILE>

Arguments:
  <IBUNDLE_FILE>  ibundle file to fetch

Options:
      --dry-run     Perform a trial fetch without making changes to the repository
      --force       Force fetch operation
  -h, --help        Print help information
  -V, --version     Print version information
  -v, --verbose...  More output per occurrence
  -q, --quiet...    Less output per occurrence
```

With `--dry-run`, a fetch operation is simulated but no changes will be made to
the repository.  This is useful for checking the validity of an ibundle file and
for testing.

git-ibundle is cautious about fetching from an unexpected bundle.  Use `--force`
to override this caution.  `--force` may be used in these cases:

- The repository is non-empty but no prior fetch has been done and thus no
  git-ibundle repo_id exists.  Without `--force`, git-ibundle will not risk
  overwriting the references of the wrong repository.

- A standalone ibundle with a non-zero basis sequence number is being applied to
  a repository that lacks that basis.  Because the ibundle is standalone, the
  set of references and prerequisite commit IDs is within the ibundle itself, so
  the `fetch` operation is safe to attempt; forcing will not override the
  requirement that all commit IDs be present.

### Report status

```text
Report status

Usage: git-ibundle status [OPTIONS]

Options:
      --long        Provide longer status
  -h, --help        Print help information
  -V, --version     Print version information
  -v, --verbose...  More output per occurrence
  -q, --quiet...    Less output per occurrence
```

This provides git-ibundle status for a given repository.  For example:

```console
$ git-ibundle status
repo_id: 18450f13-4003-474a-a69e-22782ef3848f
max_seq_num: 13
next_seq_num: 14
```

The `next_seq_num` field indicates the sequence number that will be used for the
next `git-ibundle create` operation.

The `max_seq_num` field indicates the sequence number used by the most recent
`git-ibundle create` operation.

With `--long`, more detail is provided:

```console
$ git-ibundle status --long
repo_id: 18450f13-4003-474a-a69e-22782ef3848f
max_seq_num: 13
next_seq_num: 14
long_details:
  seq_num  num_refs HEAD
  1        0        refs/heads/main
  2        0        refs/heads/main
  3        5        refs/heads/main
  4        5        refs/heads/main
  5        6        refs/heads/main
  6        7        refs/heads/fix1
  7        7        refs/heads/main2
  8        7        refs/heads/main
  9        7        343f8d34eb565c0e97194604fa2c6c3ff8ba4931 (detached)
  10       7        refs/heads/main
  11       7        refs/heads/main
  12       7        refs/heads/main
  13       11       refs/heads/main
```

### Cleanup old sequence numbers

```text
Usage: git-ibundle clean [OPTIONS]

Options:
      --keep <KEEP>  Number of sequence numbers to retain [default: 20]
  -h, --help         Print help information
  -V, --version      Print version information
  -v, --verbose...   More output per occurrence
  -q, --quiet...     Less output per occurrence

```

By default, git-ibundle retains the metadata for all sequence numbers.  Use
`git-ibundle clean` to cleanup older sequence numbers.

## Comparison with Git bundles

Most of the heavy lifting done by git-ibundle is handled by Git's own bundle
functionality.  For non-incremental mirroring, Git's bundles provide a complete
solution.  For example, the following packages the entirety of a source
repository into a bundle file:

    # Run from within the source Git repository:
    git bundle create ../repo.bundle --all

Similarly, in an empty destination repository, the following command fetches
from the bundle and replicates almost the entire repository state:

    # Run from within the destination Git repository:
    git fetch --prune --force repo.bundle "*:*"

The only thing missing is the setting of `HEAD` to the appropriate symbolic
branch name, as bundle files have no means of communicating the name of that
branch.  But one additional pair of commands takes care of that.  In the source
repository, query `HEAD` via:

    $ git symbolic-ref HEAD
    refs/heads/main

Then, in the destination repository, manually set `HEAD` accordingly, e.g.:

    git symbolic-ref HEAD refs/heads/main

Git also provides a way to exclude commits from a bundle by providing them with
a leading caret (`^`).  After a single additional commit to `main` in the
source, a new bundle file can be created with just the additional commit
(assuming `main` is the only reference in the repository):

    git bundle create ../repo.bundle --all ^HEAD~

The bundle might contain headers such as the following:

    # v2 git bundle
    -9a3bbf283e30565d9ac378cb73c36ca8a417c5e0 Some commit log message
    22a3d70042ecc8bce2772bfa85eadf64adb77441 refs/heads/main
    22a3d70042ecc8bce2772bfa85eadf64adb77441 HEAD

The commit `9a3bbf2` was sent in the first bundle; it has become a prerequisite
for this incremental bundle file.  Git does a great job of distilling the set of
requested references and exclusions down to a minimal set of prerequisite
commits and changed references.

Unfortunately, Git's prerequisites must always be commits.  Annotated tags point
to tag objects, which then point to commits.  Bundle files have no way to
express a tag object as a prerequisite.

In addition, Git will remove any requested reference that points to an object
excluded by any of the `^` exclusions.  Suppose a repository containing many
commits on `main` is bundled in its entirety via:

    git bundle create ../repo.bundle --all

Now suppose the only change is to add a new branch a couple of commits back from
`HEAD`:

    git branch branch1 HEAD~2

Attempting to request that this new branch be added to a new incremental bundle
will fail:

    git bundle create ../repo.bundle branch1 ^main

This is because `branch1` points to an ancestor of `main`, and `main` has been
excluded, causing `branch1` to be excluded as well.

To perform incremental mirroring, git-ibundle uses `git bundle create` in the
source repository to create temporary bundle files at each synchronization
point.  Within the bundle are a list of prerequisite commits, a pack of new
objects, and a list of references that are new (i.e., that point to newly
created objects in the pack).  git-ibundle then extracts this information from
the bundle file and combines it with other metadata to create an ibundle file;
at the destination repository, the ibundle is combined with stored repository
metadata to reconstruct the full set of references that are written into a
temporary bundle file; this bundle is applied to the destination repository with
`git fetch --prune --force temp.bundle "*:*"`; finally, `HEAD` is set
appropriately based on the value conveyed in the ibundle file.
