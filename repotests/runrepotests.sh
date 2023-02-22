#!/bin/bash

# verbose=true
# release=true

##############################################################################

test -z "$verbose" && verbose=false
test -z "$release" && release=false

while [ $# -gt 0 ]; do
    case "$1" in
        '-v') verbose=true;;
        '--release') release=true;;
        *) printf 'Invalid arg: %q\n' "$1"; exit 1;;
    esac
    shift
done

if $verbose; then
    Q=
    QUIET=
else
    Q=-q
    QUIET=--quiet
fi

die() {
    if [ -n "$context" ]; then
        printf 'Context: %s\n' "$context"
    fi
    printf '%s\n' "$*"
    exit 1
}

step='0'
context='none'

step_str() {
    printf '%03d' "$step"
}

set_context() {
    step=$((step+1))
    context="step $(step_str): $1"
    echo "$context" > tmp/repos/context
    if $verbose; then
        printf '\n'
        printf '=======================================================\n'
        printf 'Context: %s\n' "$context"
        printf '=======================================================\n'
    fi
}

end_context() {
    cp -a tmp/repos "tmp/steps/repos.$(step_str)"
}

REPOTESTS_DIR="$PWD"

test -f "$REPOTESTS_DIR/runrepotests.sh" ||
    die 'Run from directory with `runrepotests.sh`'

target_dir="$(readlink -f "$REPOTESTS_DIR/../target/x86_64-unknown-linux-musl")"
if $release; then
    GIT_IBUNDLE="$target_dir/release/git-ibundle"
else
    GIT_IBUNDLE="$target_dir/debug/git-ibundle"
fi

export GIT_AUTHOR_NAME='author'
export GIT_AUTHOR_EMAIL='author@example.com'
export GIT_COMMITTER_NAME='committer'
export GIT_COMMITTER_EMAIL='committer@example.com'
export GIT_AUTHOR_DATE='Fri, 11 Sep 2020 12:34:56 -0400'
export GIT_COMMITTER_DATE="$GIT_AUTHOR_DATE"

must_cd() {
    cd "$1" || die "Could not change to directory $1"
}

# May invoke as `must_mkdir dirname -p`.
must_mkdir() {
    mkdir "$@" || die "Could not create directory $1"
}

must_git() {
    git "$@" || die "failed to run git " "$@"
}

must_git_q() {
    git "$@" > /dev/null || die "failed to run git " "$@"
}

# "$1" - repo
must_git_fsck() {
    # Pipe to suppress status output.
    out=$(git -C "$1" fsck |& cat)
    fsck_status="${PIPESTATUS[0]}"
    if [ "$fsck_status" != 0 ]; then
        printf '%s\n' "$out"
        die "failed git " "$@" " fsck with status $fsck_status"
    fi
}

# $1 - new repo
# $2 - orig repo
fsck_and_diff() {
    must_git_fsck "$1"
    "$REPOTESTS_DIR/../scripts/repodiff" "$1" "$2" ||
        die "fsck_and_diff failed"
}

git_ibundle() {
    (
        must_cd "$1"
        shift
        "$GIT_IBUNDLE" "$@"
    )
}

fail_git_ibundle() {
    expected_status="$1"
    shift
    out="$(git_ibundle "$@" 2>&1)"
    status="$?"
    if [ "$status" != "$expected_status" ]; then
        die "expected_status=$expected_status != status=$status; output=$out"
    fi
}

must_git_ibundle() {
    git_ibundle "$@" || die "failed git_ibundle " "$@"
}

must_git_ibundle_status() {
    out="$(git_ibundle "$@" status)"
    status="$?"
    if [ "$status" != 0 ]; then
        die "failed git_ibundle $* status"
    fi
    repo_id="$(printf '%s\n' "$out" | grep '^repo_id:')"
    repo_id="${repo_id#*: }"
    max_seq_num="$(printf '%s\n' "$out" | grep '^max_seq_num:')"
    max_seq_num="${max_seq_num#*: }"
    next_seq_num="$(printf '%s\n' "$out" | grep '^next_seq_num:')"
    next_seq_num="${next_seq_num#*: }"
}

commit_num=0
# $1 - repo
must_git_commit_file() {
    commit_num=$((commit_num + 1))
    printf 'data-%s\n' "$commit_num" >> "$1/file.txt"
    must_git -C "$1" add file.txt
    must_git -C "$1" commit $Q -m \
        "$(printf 'Commit %s\nSummary.\n\nMore\ncomments.\n' "$commit_num")"
}

if ! test -d tmp; then
    cat <<EOF
tmp/ not present; must pre-create.  Options:

1. Use normal directory:

    mkdir tmp

2. Symlink into /run/user which may be tmpfs (for non-utf8 filenames):

    mkdir /run/user/$(id -u)/git-ibundle-repotests-tmp
    ln -s /run/user/$(id -u)/git-ibundle-repotests-tmp tmp

EOF
    exit 1
fi

if touch $'tmp/test\x80' 2> /dev/null; then
    rm $'tmp/test\x80'
    non_utf8=true
else
    non_utf8=false
    echo "tmp/ lacks support for non-utf8 paths; some tests will be skipped."
fi

rm -rf tmp/steps tmp/repos tmp/repos.*
mkdir tmp/steps tmp/repos

set_context 'create repos, verify initial status'
SRC1='tmp/repos/repo1'
DST1='tmp/repos/repo1.git'
IBU1='../repo1.ibundle'
BU1='../repo1.bundle'
mkdir -p "$SRC1"
mkdir -p "$DST1"
must_git -C "$SRC1" init $Q --initial-branch main
must_git -C "$DST1" init $Q --initial-branch main --bare
out=$(must_git_ibundle "$SRC1" status)
expected_out=$'repo_id: NONE\nmax_seq_num: 0\nnext_seq_num: 1'
test "$out" = "$expected_out" ||
    die $'status had wrong output:\n'"$out"$'\nexpected:\n'"$expected_out"
must_git_ibundle_status "$SRC1"
test "$repo_id" = 'NONE' || die "wrong repo_id=$repo_id"
test "$max_seq_num" = '0' || die "wrong max_seq_num=$max_seq_num"
test "$next_seq_num" = '1' || die "wrong next_seq_num=$next_seq_num"

set_context 'ibundle the empty repo'
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
must_git_fsck "$DST1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
must_git_fsck "$DST1"
must_git_ibundle_status "$SRC1"
src_repo_id="$repo_id"
must_git_ibundle_status "$DST1"
test "$repo_id" = "$src_repo_id" || die "wrong $DST1 repo_id=$repo_id"
end_context

set_context 'still empty repo'
fail_git_ibundle 3 "$SRC1" create $Q "$IBU1"
must_git_ibundle "$SRC1" create $Q --allow-empty "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
must_git_fsck "$DST1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
must_git_fsck "$DST1"
end_context

set_context 'commits: main, branch1, tag1, atag1'
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git -C "$SRC1" branch branch1
must_git -C "$SRC1" tag tag1
must_git -C "$SRC1" tag -a -m $'Annotated Tag 1.\n\nMore\ncomments.' atag1
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'no new commits, --standalone but semantically empty'
fail_git_ibundle 3 "$SRC1" create $Q --standalone "$IBU1"
must_git_ibundle "$SRC1" create $Q --standalone --allow-empty "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'commits, -branch1, -tag1, main2, tag2, atag2, commits'
must_git_commit_file "$SRC1"
must_git -C "$SRC1" branch $Q -D branch1
must_git -C "$SRC1" branch main2
must_git_commit_file "$SRC1"
must_git_q -C "$SRC1" tag -d tag1
must_git -C "$SRC1" tag tag2
must_git -C "$SRC1" tag -a -m $'Annotated Tag 2.\n\nMore\ncomments.' atag2
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"

must_git_ibundle_status "$DST1"
saved_basis="$max_seq_num"
cp -a "$DST1" "$DST1-basis-$saved_basis"
end_context

set_context 'wrong repo_id'
echo '00000000-0000-0000-0000-000000000000' >> "$DST1/ibundle/id"
fail_git_ibundle 1 "$DST1" fetch $Q "$IBU1"
cp "$SRC1/.git/ibundle/id" "$DST1/ibundle/id"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'fix 1: fix1'
must_git -C "$SRC1" branch $Q fix1 atag1
must_git -C "$SRC1" checkout $Q fix1
must_git_commit_file "$SRC1"
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

if $non_utf8; then
    set_context 'branch: branch_non_utf8\x80'
    must_git -C "$SRC1" branch $'branch_non_utf8\x80'
    must_git_ibundle "$SRC1" create $Q "$IBU1"
    must_git_ibundle "$DST1" fetch $Q "$IBU1"
    fsck_and_diff "$DST1" "$SRC1"
    end_context
fi

set_context 'checkout main2 (same as main)'
must_git -C "$SRC1" checkout $Q main2
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'checkout main'
must_git -C "$SRC1" checkout $Q main
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'detached head (sole change)'
must_git -C "$SRC1" checkout $Q HEAD~
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'back to main (sole change)'
must_git -C "$SRC1" checkout $Q main
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'new commit with no branch'
must_git -C "$SRC1" checkout $Q "$(must_git -C "$SRC1" rev-parse HEAD)"
must_git_commit_file "$SRC1"
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
must_git -C "$SRC1" checkout $Q main
end_context

set_context 'bundle from --basis'
must_git_ibundle "$SRC1" create $Q --basis "$saved_basis" "$IBU1"
must_git_ibundle "$DST1-basis-$saved_basis" fetch $Q "$IBU1"
fsck_and_diff "$DST1-basis-$saved_basis" "$SRC1"
end_context

set_context 'Restart from --basis 0'
must_git_ibundle "$SRC1" create $Q --basis 0 "$IBU1"
# Fetch into new $DST1-from-0 repo:
mkdir "$DST1-from-0"
must_git -C "$DST1-from-0" init $Q --initial-branch anything-but-main --bare
must_git_ibundle "$DST1-from-0" fetch $Q "$IBU1"
fsck_and_diff "$DST1-from-0" "$SRC1"
# Also fetch into $DST1.
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'Restart from --basis-current'
must_git_ibundle "$SRC1" create $Q --basis-current "$IBU1"
rm -rf "$DST1/ibundle"
fail_git_ibundle 1 "$DST1" fetch $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1" --force
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'Only commits'
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git_commit_file "$SRC1"
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'branch for HEAD, then a commit to main'
must_git -C "$SRC1" branch $Q temp-branch HEAD
must_git_commit_file "$SRC1"
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'Remove temp-branch, squash two commits'
must_git -C "$SRC1" branch $Q -D temp-branch
must_git -C "$SRC1" reset $Q HEAD~2
must_git -C "$SRC1" commit $Q -am 'Squash two commits'
must_git -C "$SRC1" gc $QUIET --prune=now
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context

set_context 'Add head-n tags into the past'
must_git -C "$SRC1" tag -a -m $'ahead-1.\n\nMore\ncomments.' ahead-1 HEAD~1
must_git -C "$SRC1" tag -a -m $'ahead-2.\n\nMore\ncomments.' ahead-2 HEAD~2
must_git -C "$SRC1" tag -a -m $'ahead-3.\n\nMore\ncomments.' ahead-3 HEAD~3
must_git -C "$SRC1" tag -a -m $'ahead-4.\n\nMore\ncomments.' ahead-4 HEAD~4
must_git_ibundle "$SRC1" create $Q "$IBU1"
must_git_ibundle "$DST1" fetch $Q "$IBU1"
fsck_and_diff "$DST1" "$SRC1"
end_context
