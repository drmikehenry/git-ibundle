# History

## Version 0.2.2

- Allow refs (tags, branches) that aren't commits.

  Git itself permits tagging of arbitrary objects.  The Linux kernel source
  currently has two instances of tag objects that point to a tree object instead
  of a commit (tags `v2.6.11`, `v2.6.11-tree`).  When a ref lacks an associated
  commit, there's no prerequisite to track, so there's no reason to fail with an
  error in that case.

- Fix warnings with new stable compiler (1.75.0).

## Version 0.2.1

- Change incorrect Git minimum version specification in `README.md`.  Git
  v2.31.0+ is required for proper operation.  Ref:
  <https://github.com/drmikehenry/git-ibundle/issues/1>

## Version 0.2.0

- Restructure ibundle format to V2 for better efficiency.  This bundle file
  format is incompatible with git-ibundle versions prior to v0.2.0 (implying
  v0.2.0 must be used on both source and destination networks), but repository
  metadata (stored in `ibundle/` directory) remains compatible.

- Remove `git ibundle to-bundle` command.  Git bundle files are too limited to
  make their generation worthwhile.

- Generate a reduced set of prerequisite commits by creating a temporary bundle
  file.  Work around Git's refusal to create an empty bundle.

- Add switch `git-ibundle create --basis-current` for bootstrapping a
  pre-existing pair of mirrored repositories.

- Disallow cleaning all metadata via `git-ibundle clean --keep 0`.

- Remove `status --long` switch; use `status --verbose` instead.

- Add `show` command to examine an ibundle file.

## Version 0.1.1

- Force fetching via `git fetch --force` to allow non-fast-forward fetches (such
  as from reworked pull requests).

## Version 0.1.0

- Initial version.
