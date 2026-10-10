#!/bin/sh
# Stateful, offline subset of the herdr CLI. Never launches a pane command or agent.
set -eu
: "${CREW_TEST_STATE:?}"
caller_cwd=$(pwd)
cd "$CREW_TEST_STATE"
mkdir -p calls workspaces
i=1
while ! mkdir "calls/$i" 2>/dev/null; do i=$((i + 1)); done
printf '%s\000' "$@" > "calls/$i/args"

error() {
    printf '{"error":{"code":"%s","message":"injected failure"}}\n' "$1"
    exit 1
}
# One-shot failures, matched against the initial arguments, are logged like normal calls.
injection=
if [ -f fail-prefix ]; then
    prefix=$(cat fail-prefix)
    case "$*" in
        "$prefix"|"$prefix "*)
            injection=$(cat fail-mode)
            rm fail-prefix fail-mode
            case "$injection" in
                transport) echo 'simulated transport failure' >&2; exit 1 ;;
                denied) echo 'Error: Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }' >&2; exit 1 ;;
                malformed) echo '{"result":{}}'; exit 0 ;;
                agent_not_ready) ;; # The agent is running, but awaiting user input.
                *) error "$injection" ;;
            esac
            ;;
    esac
fi

quote() {
    # Fixture labels and paths have no newlines; preserve spaces, quotes and shell literals.
    printf '"'
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
    printf '"'
}
option() {
    wanted=$1
    shift
    while [ "$#" -gt 1 ]; do
        if [ "$1" = "$wanted" ]; then printf '%s' "$2"; return; fi
        shift
    done
    echo "missing option $wanted" >&2
    exit 99
}
tab_for_pane() {
    for t in workspaces/*/tabs/*; do
        [ -f "$t/pane" ] || continue
        if [ "$(cat "$t/pane")" = "$1" ]; then printf '%s' "$t"; return; fi
    done
    echo "unknown pane $1" >&2
    exit 99
}
new_tab() {
    t="workspaces/$1/tabs/t$i"
    mkdir -p "$t"
    quote "$2" > "$t/label"
    quote "$3" > "$t/cwd"
    printf 'p%s' "$i" > "$t/pane"
}

case "${1-} ${2-}" in
    '--version ')
        printf 'herdr %s\n' "${CREW_TEST_HERDR_VERSION:-0.9.3}"
        ;;
    'status server')
        printf '{"version":"0.9.3","socket":%s}\n' "$(quote "${CREW_TEST_SOCKET:-/offline-herdr.sock}")"
        ;;
    'pane get')
        t=$(tab_for_pane "$3")
        w=${t#workspaces/}; w=${w%%/*}
        printf '{"result":{"pane":{"pane_id":"%s","terminal_id":"term-%s","workspace_id":"%s","tab_id":"%s","cwd":%s}}}\n' "$3" "$3" "$w" "${t##*/}" "$(cat "$t/cwd")"
        ;;
    'server ')
        # ensure_server must remove the nesting markers before spawning the server.
        printf '%s|%s|%s' "${CLAUDECODE-unset}" "${CLAUDE_CODE_CHILD_SESSION-unset}" "${CLAUDE_CODE_ENTRYPOINT-unset}" > server-env
        printf '%s' "$caller_cwd" > server-cwd
        touch online
        echo '{"result":{}}'
        ;;
    'workspace list')
        [ -f online ] || error no_server
        printf '{"result":{"workspaces":['
        sep=
        for w in workspaces/*; do
            [ -f "$w/label" ] || continue
            printf '%s{"workspace_id":"%s","label":%s}' "$sep" "${w##*/}" "$(cat "$w/label")"
            sep=,
        done
        echo ']}}'
        ;;
    'workspace create')
        w="w$i"
        label=$(option --label "$@")
        cwd=$(option --cwd "$@")
        mkdir -p "workspaces/$w"
        quote "$label" > "workspaces/$w/label"
        new_tab "$w" 1 "$cwd"
        printf '{"result":{"workspace":{"workspace_id":"%s"},"tab":{"tab_id":"t%s"},"root_pane":{"pane_id":"p%s"}}}\n' "$w" "$i" "$i"
        ;;
    'workspace rename') quote "$4" > "workspaces/$3/label"; echo '{"result":{}}' ;;
    'workspace focus'|'notification show') echo '{"result":{}}' ;;
    'tab list'|'pane list')
        w=$(option --workspace "$@")
        printf '{"result":{"%ss":[' "$1"
        sep=
        for t in "workspaces/$w/tabs/"*; do
            [ -f "$t/label" ] || continue
            if [ "$1" = tab ]; then
                printf '%s{"tab_id":"%s","label":%s}' "$sep" "${t##*/}" "$(cat "$t/label")"
            else
                printf '%s{"pane_id":"%s","terminal_id":"term-%s","workspace_id":"%s","tab_id":"%s","cwd":%s' "$sep" "$(cat "$t/pane")" "$(cat "$t/pane")" "$w" "${t##*/}" "$(cat "$t/cwd")"
                if [ -f "$t/session" ]; then printf ',"agent_session":{"value":"%s","agent":"%s","kind":"id","source":"herdr:%s"}' "$(cat "$t/session")" "$(cat "$t/kind" 2>/dev/null || echo claude)" "$(cat "$t/kind" 2>/dev/null || echo claude)"; fi
                printf '}'
            fi
            sep=,
        done
        echo ']}}'
        ;;
    'tab create')
        w=$(option --workspace "$@")
        new_tab "$w" "$(option --label "$@")" "$(option --cwd "$@")"
        printf '{"result":{"tab":{"tab_id":"t%s"},"root_pane":{"pane_id":"p%s"}}}\n' "$i" "$i"
        ;;
    'tab rename'|'tab close')
        found=false
        for t in workspaces/*/tabs/"$3"; do
            [ -d "$t" ] || continue
            found=true
            if [ "$2" = rename ]; then quote "$4" > "$t/label"; else rm -rf "$t"; fi
        done
        [ "$found" = true ] || exit 99
        echo '{"result":{}}'
        ;;
    'agent list')
        printf '{"result":{"agents":['
        sep=
        for t in workspaces/*/tabs/*; do
            [ -f "$t/agent" ] || continue
            printf '%s%s' "$sep" "$(cat "$t/agent")"
            sep=,
        done
        echo ']}}'
        ;;
    'agent start')
        pane=$(option --pane "$@")
        t=$(tab_for_pane "$pane")
        w=${t#workspaces/}; w=${w%%/*}
        kind=$(option --kind "$@")
        if [ "$kind" = claude ]; then
            prompt=$(option --append-system-prompt-file "$@")
            [ -s "$prompt" ] || { echo 'prompt missing at agent start' >&2; exit 99; }
        fi
        if [ "$kind" = pi ]; then
            prompt=$(option --herdr-crew-prompt "$@")
            [ -s "$prompt" ] || { echo 'prompt missing at agent start' >&2; exit 99; }
        fi
        printf '%s' "$kind" > "$t/kind"
        printf '{"name":%s,"kind":"%s","workspace_id":"%s","tab_id":"%s","pane_id":"%s"}' "$(quote "$3")" "$kind" "$w" "${t##*/}" "$pane" > "$t/agent"
        printf 'session-%s' "$pane" > "$t/session"
        touch "$t/running"
        if [ "$injection" = agent_not_ready ]; then error agent_not_ready; fi
        echo '{"result":{}}'
        ;;
    'agent get')
        t=$(tab_for_pane "$3")
        [ -f "$t/agent" ] || error agent_not_found
        printf '{"result":{"agent":{"pane_id":"%s","agent_status":"%s"}}}\n' "$3" "$(cat "$t/status" 2>/dev/null || echo idle)"
        ;;
    'agent prompt')
        t=$(tab_for_pane "$3")
        [ -f "$t/agent" ] || error agent_not_found
        [ "$(cat "$t/status" 2>/dev/null || echo idle)" != blocked ] || error agent_blocked
        printf '%s\000' "$4" >> "$t/prompts"
        echo '{"result":{}}'
        ;;
    'pane process-info')
        t=$(tab_for_pane "$(option --pane "$@")")
        group=1
        [ ! -f "$t/running" ] || group=2
        printf '{"result":{"process_info":{"shell_pid":1,"foreground_process_group_id":%s,"foreground_processes":[{"name":"simulated-process"}]}}}\n' "$group"
        ;;
    'pane run')
        t=$(tab_for_pane "$3")
        printf '%s' "$4" > "$t/command"
        touch "$t/running"
        echo '{"result":{}}'
        ;;
    *) echo "unsupported simulated herdr command: $*" >&2; exit 99 ;;
esac
