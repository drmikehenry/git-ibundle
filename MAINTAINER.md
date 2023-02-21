# Maintainer's guide

## Setup to build

- Add targets for MUSL and Windows:

      rustup target add x86_64-unknown-linux-musl
      rustup target add x86_64-pc-windows-gnu

- Provide native toolchains; on Ubuntu:

      apt-get install -y musl-dev musl-tools
      apt-get install -y gcc-mingw-w64-x86-64

## Making a release

- Verify version is correct in `Cargo.toml`.

- Ensure everything is committed.

- Run tests.

- Test packaging, verifying the package contents:

      cargo package --list

- Build release binaries for Linux and Windows:

      ./scripts/make_release

- Create a tag as prompted by the `make_release` script, e.g.:

      If all looks good, tag this release:

        git tag -am "Release v0.1.1." v0.1.1

- Push to Github.

- Create a release at Github:

  - Visit <https://github.com/drmikehenry/git-ibundle/releases>.

  - Draft a new release.

  - Upload archives from `target/github/`:

        git-ibundle-0.1.1-x86_64-unknown-linux-musl.tar.gz
        git-ibundle-0.1.1-x86_64-pc-windows-gnu.zip

  - Add notes from `CHANGES.md` as desired.

- Publish the package to crates.io:

      cargo publish

## Reference information

### Git pack format

- pack format: <https://git-scm.com/docs/pack-format>

  > 4-byte signature: The signature is: `{'P', 'A', 'C', 'K'}`
  >
  > 4-byte version number (network byte order): Git currently accepts
  > version number 2 or 3 but generates version 2 only.
  >
  > 4-byte number of objects contained in the pack (network byte order)
  >
  > The header is followed by number of object entries
  >
  > The trailer records 20-byte SHA-1 checksum of all of the above.

  This is an empty pack:

      5041 434b 0000 0002 0000 0000 029d 0882  PACK............
      3bd8 a8ea b510 ad6a c75c 823c fd3e d31e  ;......j.\.<.>..

      echo '0: 5041434b0000000200000000' | xxd -r | sha1sum

      029d08823bd8a8eab510ad6ac75c823cfd3ed31e  -

  It's truly empty, just a V2 Pack file with zero entries.

- Can create an empty pack file `empty.pack` by making an empty PACK object to
  stdout, e.g.:

      git pack-objects --stdout < /dev/null > ../empty.pack

### Creating a Git pack

- Objects should be packed in "recency" order; from
  `libgit2/include/git2/pack.h`:

  Creation of packfiles requires two steps:

  - First, insert all the objects you want to put into the packfile
    using `git_packbuilder_insert` and `git_packbuilder_insert_tree`.
    It's important to add the objects in recency order ("in the order
    that they are 'reachable' from head").

    "ANY order will give you a working pack, ... [but it is] the thing
    that gives packs good locality. It keeps the objects close to the
    head (whether they are old or new, but they are _reachable_ from the
    head) at the head of the pack. So packs actually have absolutely
    _wonderful_ IO patterns." - Linus Torvalds
    git.git/Documentation/technical/pack-heuristics.txt

    (N.B. The `pack-heuristics.txt` file is entertaining.)

  - Second, use `git_packbuilder_write` or `git_packbuilder_foreach` to
    write the resulting packfile.

    libgit2 will take care of the delta ordering and generation.
    `git_packbuilder_set_threads` can be used to adjust the number of
    threads used for the process.

  See tests/pack/packbuilder.c for an example.

  The example has `git_revwalk_sorting(_revwalker, GIT_SORT_TIME);`.

### Git bundle format

- Git bundle format: <https://git-scm.com/docs/gitformat-bundle>

      bundle    = signature *capability *prerequisite *reference LF pack
      signature = "# v3 git bundle" LF

      capability   = "@" key ["=" value] LF
      prerequisite = "-" obj-id SP comment LF
      comment      = *CHAR
      reference    = obj-id SP refname LF
      key          = 1*(ALPHA / DIGIT / "-")
      value        = *(%01-09 / %0b-FF)

      pack         = ... ; packfile

  Only two capabilities:

  - `object-format`: specifies the hash algorithm in use, and can take the
    same values as the `extensions.objectFormat` configuration value.

  - `filter`: specifies an object filter as in the `--filter` option in
    `git-rev-list`. The resulting pack-file must be marked as a `.promisor`
    pack-file after it is unbundled.

- Git bundle prerequisites must be commits; tag objects are not valid
  as prerequisites.

- Git doesn't seem to like a bundle file that has no references at all.
  Without references, it's not willing to unpack a non-empty PACK section.

- Normally, the only references in a bundle file created by Git will refer to
  objects created by the bundle's pack; but `git fetch some.bundle` is willing
  to create any references mentioned in the bundle header, even if the
  associated objects aren't created by the pack but already exist in the repo.

- git bundle uses these calls (loosely):

      git rev-list --boundary --pretty=oneline <argv> |
        git pack-objects --all-progress-implied --stdout --thin \
        --delta-base-offset

- Git bundle format has no method for indicating the name of the `HEAD` branch.
  Instead, it lists commit id + name for tags, branches, and `HEAD`, e.g.:

    d442ad93b53dac93e52880c057ac7e733ec50db4 refs/heads/br2
    d442ad93b53dac93e52880c057ac7e733ec50db4 refs/heads/br3
    b826f67c769e3a728729653974dac11279cafa1c refs/heads/feature1
    d442ad93b53dac93e52880c057ac7e733ec50db4 refs/heads/master
    d442ad93b53dac93e52880c057ac7e733ec50db4 refs/heads/ver2
    d442ad93b53dac93e52880c057ac7e733ec50db4 HEAD

  During `git clone some.bundle`, Git therefore uses heuristics in
  `guess_remote_head()` to set an initial symbolic value for `HEAD`.  First, if
  the bundle's references contain a branch matching the user's default
  configured branch name and that branch has the same OID as `HEAD`, Git sets
  `HEAD` symbolically to that branch.  Failing that, if `refs/heads/master` is
  found with a matching OID, Git chooses that branch name.  Lastly, it probes
  for the first other branch name with a matching OID.

### Git empty bundles

- Normally, `git bundle create` refuses to create an "empty" bundle, by which it
  means a bundle that has no packed objects.

- The bundle format itself can represent an empty bundle.

- Can synthesize an empty bundle by making an empty PACK file as shown above and
  appending that to hand-crafted bundle header lines.

- `git bundle create` throws away any specified refs that point to pre-existing
  objects.  Even if such a ref is "new" in the sense that it wasn't present last
  time we synchronized into the destination repository, if it points into "the
  past", Git will throw it out of the bundle.

### Error `Ignoring funny ref 'HEAD' locally`

- Can get the following error when doing `git fetch` into a repo with a detached
  head (e.g., `HEAD` is set to a commit-ish OID directly, rather than being
  symbolic):

      error: * Ignoring funny ref 'HEAD' locally

  Seems harmless; demonstrate via:

      mkdir -p ~/tmp/git/testing-funny-head
      cd ~/tmp/git/testing-funny-head
      rm -rf repo repo2.git repo.bundle
      mkdir repo
      git -C repo init -q
      date >> repo/file.txt
      git -C repo add file.txt
      git -C repo commit -qm 'commit 1.'
      date >> repo/file.txt
      git -C repo add file.txt
      git -C repo commit -qm 'commit 2.'
      git -C repo branch branch1
      git -C repo checkout -q HEAD~
      git -C repo bundle create ../repo.bundle --all
      git clone --bare -q repo.bundle repo2.git
      git -C repo2.git fetch ../repo.bundle '*:*'

  Restoring to a symbolic HEAD fixes the error:

      git -C repo2.git symbolic-ref HEAD refs/heads/main
      git -C repo2.git fetch ../repo.bundle '*:*'

### Detached head with unique OID

Within a Git bundle, if `HEAD` is the only reference to an object in the pack,
that object is not unpacked (or at least not retained) after a `git fetch` of
the bundle.

Consider a repo with at least one commit:

    mkdir head-test
    cd head-test
    git init
    date >> date.txt
    git add date.txt
    git commit -m 'First commit.'

Create a bundle containing only `HEAD` with all of the commits on the associated
branch:

    git bundle create ../head-only.bundle HEAD

This yields a bundle with no prereqs and a single reference, `HEAD`, e.g.:

    # v2 git bundle
    c6d6563d1260f395f03e717979e839835dc4ad93 HEAD

Git will fetch from this bundle into an empty repository without error, e.g.:

    mkdir ../fetch-head.git
    git -C ../fetch-head.git init --bare
    git -C ../fetch-head.git fetch ../head-only.bundle '*:*'

No references are shown, and the object is not found:

    $ git -C ../fetch-head.git show-ref

    $ git -C ../fetch-head.git log c6d6563d1260f395f03e717979e839835dc4ad93
    fatal: bad object c6d6563d1260f395f03e717979e839835dc4ad93

No ancestor objects are found, either, though they were also in the pack.

If the bundle file `head-only.bundle` is edited in-place to chane `HEAD` to
`refs/heads/main` in the headers while retaining the pack as-is, the fetch
operation now works: the objects are unpacked and available in the destination
repository.  This indicates that the branch name is important, and that `HEAD`
is insufficient to ensure proper object unpacking.

Typically, `HEAD` will not be the only reference to a packed object (since
`HEAD` is typically a symbolic reference to a named branch), but it's possible
to construct a detached-head scenario where this happens.  One way is to start
with a repository with at least one commit, make `main.bundle` representing the
`main` branch so far, then checkout the latest commit in detached-head mode and
add a new commit; starting from `head-test` above:

    git bundle create ../main.bundle main
    git checkout "$(git rev-parse HEAD)"
    date >> date.txt
    git commit -am 'Second commit (with no named branch).'

An incremental Git bundle may be created by excluding the original `main` and
keeping the changes in `HEAD`:

    git bundle create ../incremental.bundle HEAD ^main

The headers of `all.bundle` show that `HEAD` differs from `main`:

    # v2 git bundle
    c6d6563d1260f395f03e717979e839835dc4ad93 refs/heads/main
    48d206f26f914ea43494e8b6ceab91bbf039c7dc HEAD

Fetching both bundles into a new bare repository mirroring `head-test` succeeds
without error:

    mkdir ../head-test.git
    git -C ../head-test.git init --bare
    git -C ../head-test.git fetch ../main.bundle '*:*'
    git -C ../head-test.git fetch ../incremental.bundle '*:*'

But as before, the new commit is missing:

    $ git -C ../head-test.git log 48d206f26f914ea43494e8b6ceab91bbf039c7dc
    fatal: bad object 48d206f26f914ea43494e8b6ceab91bbf039c7dc

A work-around for this behavior is to replace the name `HEAD` with a temporary
branch name before fetching, then delete the temporary branch name afterward.
For uniqueness, the branch could be named `refs/heads/HEAD-<OID>`, e.g.:

    git branch HEAD-48d206f26f914ea43494e8b6ceab91bbf039c7dc
    git bundle create ../incremental2.bundle \
      HEAD-48d206f26f914ea43494e8b6ceab91bbf039c7dc ^main
    git branch -d HEAD-48d206f26f914ea43494e8b6ceab91bbf039c7dc

    git -C ../head-test.git fetch ../incremental2.bundle '*:*'
    $ git -C ../head-test.git log 48d206f26f914ea43494e8b6ceab91bbf039c7dc

    <log output>

    git -C ../head-test.git branch \
      -d HEAD-48d206f26f914ea43494e8b6ceab91bbf039c7dc

### Git symbolic references

Git permits the creation of symbolic references (other than just `HEAD`).  It's
possible to create one by hand for testing; but since `git clone` fails to
preserve these references as symbolic, it seems unlikely that such symbolic
references will come up in the context of repository mirroring.

Here's an example creating a symbolic reference named `refs/heads/symref` which
points to a branch `refs/heads/direct`:

      mkdir -p ~/tmp/git/test-symref
      cd ~/tmp/git/test-symref
      rm -rf symref*
      mkdir symref
      git -C symref init
      date > symref/date.txt
      git -C symref add date.txt
      git -C symref commit -qm 'Add `date.txt`.'
      git -C symref branch direct
      git -C symref symbolic-ref refs/heads/symref refs/heads/direct
      git clone -q --bare symref symref.git

In the original repository, `refs/heads/symref` is symbolic:

    $ git -C symref symbolic-ref refs/heads/symref
    refs/heads/direct

But in the cloned repository, it's not:

    $ git -C symref.git symbolic-ref refs/heads/symref
    fatal: ref refs/heads/symref is not a symbolic ref

In both cases, the branch refers to the same commit:

    $ git -C symref show-ref refs/heads/symref
    9a3bbf283e30565d9ac378cb73c36ca8a417c5e0 refs/heads/symref

    $ git -C symref.git show-ref refs/heads/symref
    9a3bbf283e30565d9ac378cb73c36ca8a417c5e0 refs/heads/symref

### Additional notes

- Good answer about force and fetch:
  <https://stackoverflow.com/questions/50626560/git-fetch-non-fast-forward-update>

- After-the-fact, can delete the `refs/pull` tree from a Git repo:

      cd repo.git

      git for-each-ref refs/pull/ --format='delete %(refname)' |
        git update-ref --stdin

  Then prevent future fetching via:

      git config --add remote.origin.fetch '^refs/pull/*'
