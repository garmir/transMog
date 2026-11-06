#!/usr/bin/env bash
# deploy/deploy.sh [--dry-run] [--local-ci] <commit>
#
# builds a commit that integration's ci passed, moves the transmog user
# unit onto it and checks it; when a gate fails it puts the previous build
# back, checks that the same way and exits non-zero. the readme's deploy
# section walks through it, deploy.env.example has every setting.
#
# exit status: 0 deployed, or planned with --dry-run; 1 a gate failed and
# the previous build passed its gates again; 2 the previous build failed
# them too; 3 refused or failed before the unit was touched
set -euo pipefail

usage() {
	cat <<'EOF'
usage: deploy/deploy.sh [--dry-run] [--local-ci] <commit>

  --dry-run   resolve, check ci, build and stage the binary as
              BIN_DIR/staged, print the plan; the unit is not touched
  --local-ci  run `nix flake check` at the commit instead of asking
              github for the commit's integration runs

settings come from $DEPLOY_ENV, by default
${XDG_CONFIG_HOME:-$HOME/.config}/transmog/deploy.env
EOF
}

# ---------------------------------------------------------------- logging

stamp() { date '+%F %T.%3N'; }

log() {
	local line
	line="$(stamp) $*"
	printf '%s\n' "$line" >&2
	if [[ -n ${DEPLOY_LOG:-} ]]; then
		printf '%s\n' "$line" >>"$DEPLOY_LOG"
	fi
}

# a one-line summary: logged, and handed to LOG_HOOK as $1
summary() {
	log "== $*"
	if [[ -n ${LOG_HOOK:-} ]]; then
		timeout 10 bash -c "$LOG_HOOK" log-hook "$*" </dev/null >/dev/null 2>&1 ||
			log "log hook failed"
	fi
}

refuse() {
	summary "deploy refused: $*"
	exit 3
}

# runs a command with its output in the log, line by line
run() {
	log "+ $*"
	local status=0
	# with pipefail the loop, which never fails, leaves the command's status
	"$@" 2>&1 | while IFS= read -r line; do log "  $line"; done || status=$?
	return "$status"
}

# ---------------------------------------------------------------- settings

load_env() {
	DEPLOY_ENV=${DEPLOY_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/transmog/deploy.env}
	if [[ ! -r $DEPLOY_ENV ]]; then
		printf 'deploy: no settings at %s; copy deploy/deploy.env.example there\n' \
			"$DEPLOY_ENV" >&2
		exit 3
	fi
	# shellcheck source=/dev/null
	source "$DEPLOY_ENV"

	local state=${XDG_STATE_HOME:-$HOME/.local/state}/transmog
	: "${STATE_DIR:=$state/deploy}"
	: "${DEPLOY_LOG:=$state/deploy.log}"
	: "${PAUSE_FILE:=}"
	: "${PAUSE_TIMEOUT:=1800}"
	: "${LOG_HOOK:=}"
	: "${CI_BRANCH:=integration}"
	: "${HEALTH_SECS:=20}"
	: "${STARTUP_SECS:=15}"
	: "${MIN_FPS:=29}"
	: "${MAX_SEQ_GAPS:=10}"
	: "${PORT_WAIT_SECS:=10}"
	: "${NDI_PORTS=5960 5961}"
	: "${HEALTH_CMD_TIMEOUT:=30}"
	: "${TAP_DIR:=}"
	: "${NIX_FLAGS:=}"
	read -ra NIX_ARGS <<<"$NIX_FLAGS"
	[[ -v HEALTH_CMDS ]] || HEALTH_CMDS=()
	[[ -v UNIT_PROPS ]] || UNIT_PROPS=()
	[[ -v RESTART_UNITS ]] || RESTART_UNITS=()

	[[ -n ${BIN_DIR:-} ]] || { echo "deploy: BIN_DIR is not set in $DEPLOY_ENV" >&2; exit 3; }
	[[ -n ${UNIT:-} ]] || { echo "deploy: UNIT is not set in $DEPLOY_ENV" >&2; exit 3; }
	UNIT=${UNIT%.service}
	mkdir -p "$BIN_DIR" "$STATE_DIR" "$(dirname "$DEPLOY_LOG")"
}

# ---------------------------------------------------------------- the unit

# what the unit runs, as BIN, ARGV and PROPS (`Key=Value` lines for
# systemd-run -p), taken from the unit's transient file when it is
# loaded, else from the copy saved at the last start; UNIT_ARGS in the env
# file replaces the arguments and UNIT_PROPS adds to RUN_PROPS
SPEC_BIN=
SPEC_ARGV=()
SPEC_PROPS=()
SPEC_FROM=
RUN_PROPS=()

read_unit_file() {
	local frag
	frag=$(systemctl --user show -P FragmentPath "$UNIT.service" 2>/dev/null) || return 1
	# a unit with a file of its own would have to name BIN_DIR/current in
	# ExecStart; only systemd-run units are handled
	[[ $frag == */systemd/transient/* && -r $frag ]] || return 1
	local section='' line exec=''
	SPEC_PROPS=()
	while IFS= read -r line; do
		case $line in
		'['*']')
			section=$line
			continue
			;;
		'' | '#'*) continue ;;
		esac
		[[ $section == '[Service]' ]] || continue
		case $line in
		ExecStart=) ;;
		ExecStart=*) exec=${line#ExecStart=} ;;
		*) SPEC_PROPS+=("$line") ;;
		esac
	done <"$frag"
	[[ -n $exec ]] || return 1
	# systemd writes each word double quoted; xargs undoes that, which
	# covers paths and urls but not backslash escapes, so arguments that
	# need them go in UNIT_ARGS
	local words=()
	readarray -d '' words < <(xargs printf '%s\0' <<<"$exec")
	((${#words[@]} > 0)) || return 1
	SPEC_BIN=${words[0]}
	SPEC_ARGV=("${words[@]:1}")
	SPEC_FROM=$frag
}

read_saved_spec() {
	[[ -r $STATE_DIR/unit.argv && -r $STATE_DIR/unit.props ]] || return 1
	local words=()
	readarray -d '' words <"$STATE_DIR/unit.argv"
	((${#words[@]} > 0)) || return 1
	SPEC_BIN=${words[0]}
	SPEC_ARGV=("${words[@]:1}")
	readarray -d '' SPEC_PROPS <"$STATE_DIR/unit.props"
	SPEC_FROM=$STATE_DIR/unit.argv
}

read_spec() {
	read_unit_file || read_saved_spec ||
		[[ -v UNIT_ARGS ]] ||
		refuse "$UNIT is not a loaded systemd-run unit and no saved or UNIT_ARGS arguments exist"
	if [[ -v UNIT_ARGS ]]; then
		SPEC_ARGV=("${UNIT_ARGS[@]}")
		SPEC_FROM="${SPEC_FROM:+$SPEC_FROM, }UNIT_ARGS"
	fi
	if [[ -z $SPEC_BIN && -L $BIN_DIR/current ]]; then
		SPEC_BIN=$(readlink "$BIN_DIR/current")
	fi
	RUN_PROPS=("${SPEC_PROPS[@]}" "${UNIT_PROPS[@]}")
	derive_paths
}

save_spec() {
	printf '%s\0' "$SPEC_BIN" "${SPEC_ARGV[@]}" >"$STATE_DIR/unit.argv"
	if ((${#SPEC_PROPS[@]} > 0)); then
		printf '%s\0' "${SPEC_PROPS[@]}" >"$STATE_DIR/unit.props"
	else
		: >"$STATE_DIR/unit.props"
	fi
}

# the tap dir and control.state follow transmog's own rules: `--tap DIR`,
# else $XDG_RUNTIME_DIR/transmog/tap; control.state beside --control-file,
# else in the tap dir
CONTROL_STATE=
CONTROL_WS=
derive_paths() {
	local i tap='' file=''
	CONTROL_WS=
	CONTROL_STATE=
	for ((i = 0; i < ${#SPEC_ARGV[@]}; i++)); do
		case ${SPEC_ARGV[i]} in
		--tap)
			# DIR is optional; gui takes no positionals, so any next word is it
			if [[ -n ${SPEC_ARGV[i + 1]:-} && ${SPEC_ARGV[i + 1]} != -* ]]; then
				tap=${SPEC_ARGV[i + 1]}
			fi
			;;
		--tap=*) tap=${SPEC_ARGV[i]#--tap=} ;;
		--control) CONTROL_WS=${SPEC_ARGV[i + 1]:-} ;;
		--control=*) CONTROL_WS=${SPEC_ARGV[i]#--control=} ;;
		--control-file) file=${SPEC_ARGV[i + 1]:-} ;;
		--control-file=*) file=${SPEC_ARGV[i]#--control-file=} ;;
		esac
	done
	if [[ -z $TAP_DIR ]]; then
		TAP_DIR=${tap:-${XDG_RUNTIME_DIR:-/tmp}/transmog/tap}
	fi
	if [[ -n $file ]]; then
		CONTROL_STATE=$file.state
	elif [[ -n $CONTROL_WS ]]; then
		CONTROL_STATE=$TAP_DIR/control.state
	fi
}

unit_show() {
	systemctl --user show -p ActiveState -p MainPID -p NRestarts "$UNIT.service" 2>/dev/null
}

# sets U_STATE, U_PID, U_RESTARTS
unit_status() {
	U_STATE='' U_PID=0 U_RESTARTS=0
	local line
	while IFS= read -r line; do
		case $line in
		ActiveState=*) U_STATE=${line#*=} ;;
		MainPID=*) U_PID=${line#*=} ;;
		NRestarts=*) U_RESTARTS=${line#*=} ;;
		esac
	done < <(unit_show)
}

ports_busy() {
	local p
	for p in $NDI_PORTS; do
		[[ -z $(ss -Htan "( sport = :$p )") ]] || return 0
	done
	return 1
}

stop_unit() {
	unit_status
	if [[ $U_STATE == active || $U_STATE == activating || $U_STATE == reloading ]]; then
		run systemctl --user stop "$UNIT.service" || log "stop returned non-zero"
	fi
	systemctl --user reset-failed "$UNIT.service" >/dev/null 2>&1 || true
	# the ndi sdk takes the next free port when 5961 is still held by the
	# old socket, and the ssh tunnel carries only 5960 and 5961
	local deadline=$((SECONDS + PORT_WAIT_SECS))
	while ports_busy; do
		if ((SECONDS >= deadline)); then
			log "ports $NDI_PORTS still in use after ${PORT_WAIT_SECS}s:"
			ss -Htanp | grep -E ":($(tr ' ' '|' <<<"$NDI_PORTS")) " | while IFS= read -r l; do log "  $l"; done || true
			return 0
		fi
		sleep 0.1
	done
	[[ -z $NDI_PORTS ]] || log "ports $NDI_PORTS free"
}

SWAP_PID=0
SWAP_RESTARTS=0
SWAP_MARK=

start_unit() {
	local bin=$1 props=() p
	for p in "${RUN_PROPS[@]}"; do props+=(-p "$p"); done
	SWAP_MARK=$STATE_DIR/started
	touch "$SWAP_MARK"
	run systemd-run --user --unit "$UNIT" "${props[@]}" "$bin" "${SPEC_ARGV[@]}" ||
		return 1
	unit_status
	SWAP_PID=$U_PID
	SWAP_RESTARTS=$U_RESTARTS
	log "$UNIT started on $bin, pid $SWAP_PID"
	[[ $SWAP_PID != 0 ]]
}

restart_followers() {
	local u
	for u in "${RESTART_UNITS[@]}"; do
		run systemctl --user restart "$u" || summary "restart of $u failed"
	done
}

# ---------------------------------------------------------------- gates

GATE_NOTES=()

alive() {
	unit_status
	[[ $U_STATE == active && $U_PID == "$SWAP_PID" && $U_RESTARTS == "$SWAP_RESTARTS" ]] &&
		kill -0 "$SWAP_PID" 2>/dev/null
}

in_unit() {
	[[ $1 == "$SWAP_PID" ]] || grep -q "/$UNIT.service\$" "/proc/$1/cgroup" 2>/dev/null
}

tap_latest() {
	local seq slots
	read -r seq slots <"$TAP_DIR/latest" 2>/dev/null || return 1
	[[ $seq =~ ^[0-9]+$ && $slots =~ ^[0-9]+$ ]] || return 1
	printf '%s %s\n' "$seq" "$slots"
}

# `SEQUENCE TIMESTAMP_NS` of slot SEQ, the v4l2 sequence and kernel
# timestamp, when the ring has not moved past it
slot_header() {
	local f lo hi v _flags tlo thi
	f=$(printf '%s/%03d.frame' "$TAP_DIR" $(($1 % $2)))
	read -r lo hi v _flags tlo thi < <(od -An -tu4 -w24 -j8 -N24 -v "$f" 2>/dev/null) || return 1
	[[ -n ${thi:-} ]] && (($1 == lo + hi * 4294967296)) || return 1
	printf '%s %s\n' "$v" $((tlo + thi * 4294967296))
}

gate_fail() {
	GATE_NOTES+=("$*")
	log "gate failed: $*"
}

# waits for the new process's tap, restarts the followers, then watches
# for HEALTH_SECS; fails on the first dead process, else checks them all
gates() {
	local label=$1
	GATE_NOTES=()

	local deadline=$((SECONDS + STARTUP_SECS))
	while ! [[ -e $TAP_DIR/latest && $TAP_DIR/latest -nt $SWAP_MARK ]]; do
		if ! alive; then
			gate_fail "process: $UNIT is $U_STATE, pid $U_PID before the tap came up"
			return 1
		fi
		if ((SECONDS >= deadline)); then
			gate_fail "tap: no fresh $TAP_DIR/latest within ${STARTUP_SECS}s"
			return 1
		fi
		sleep 0.2
	done
	log "tap up in $TAP_DIR"
	restart_followers

	# fps comes from the kernel timestamps of the first and last slot read,
	# so the poll interval and the time the reads take do not enter it
	local seq slots last v ts prev='' gaps=0 s first='' first_ts=0 end_seq=0 end_ts=0
	read -r last slots < <(tap_latest) || {
		gate_fail "tap: unreadable latest"
		return 1
	}
	local end=$((SECONDS + HEALTH_SECS))
	while ((SECONDS < end)); do
		sleep 0.5
		if ! alive; then
			gate_fail "process: $UNIT is $U_STATE, pid $U_PID (was $SWAP_PID), restarts $U_RESTARTS"
			return 1
		fi
		read -r seq slots < <(tap_latest) || continue
		if ((seq < last)); then
			gate_fail "tap: seq went back from $last to $seq"
			return 1
		fi
		for ((s = last + 1; s <= seq; s++)); do
			if read -r v ts < <(slot_header "$s" "$slots"); then
				if [[ -z $first ]]; then
					first=$s
					first_ts=$ts
				fi
				end_seq=$s
				end_ts=$ts
				if [[ -n $prev ]] && ((v > prev + 1)); then
					gaps=$((gaps + v - prev - 1))
				fi
				prev=$v
			else
				prev=''
			fi
		done
		last=$seq
	done

	local fps frames=$((end_seq - ${first:-0}))
	fps=$(awk -v n="$frames" -v d=$((end_ts - first_ts)) \
		'BEGIN { printf "%.2f", (d > 0 ? n * 1e9 / d : 0) }')
	log "tap: $frames frames, $fps fps, $gaps v4l2 seq gaps over ${HEALTH_SECS}s"
	if awk -v f="$fps" -v m="$MIN_FPS" 'BEGIN { exit !(f < m) }'; then
		gate_fail "tap: $fps fps, below $MIN_FPS"
	fi
	if ((gaps > MAX_SEQ_GAPS)); then
		gate_fail "tap: $gaps v4l2 seq gaps, over $MAX_SEQ_GAPS"
	fi

	local p pid pids ok port_bad=no
	for p in $NDI_PORTS; do
		pids=$(ss -Htlnp "( sport = :$p )" | grep -o 'pid=[0-9]*' | cut -d= -f2 | sort -u || true)
		ok=yes
		[[ -n $pids ]] || ok=no
		for pid in $pids; do in_unit "$pid" || ok=no; done
		if [[ $ok == yes ]]; then
			log "port $p: held by $UNIT"
		else
			gate_fail "port $p: listener ${pids:-none}, not $UNIT pid $SWAP_PID"
			port_bad=yes
		fi
	done
	if [[ $port_bad == yes ]]; then
		ss -Htlnp | grep "pid=$SWAP_PID," | while IFS= read -r l; do log "  $UNIT listens: $l"; done || true
	fi

	if [[ -n $CONTROL_STATE ]]; then
		if [[ ! -e $CONTROL_STATE || ! $CONTROL_STATE -nt $SWAP_MARK ]]; then
			gate_fail "control: $CONTROL_STATE not written since the start"
		elif [[ -n $CONTROL_WS ]] && ! grep -qxF "ws $CONTROL_WS connected" "$CONTROL_STATE"; then
			gate_fail "control: $(grep '^ws ' "$CONTROL_STATE" || echo 'no ws line') in $CONTROL_STATE"
		else
			log "control: $CONTROL_STATE fresh"
		fi
	fi

	local cmd
	for cmd in "${HEALTH_CMDS[@]}"; do
		if run env DEPLOY_PID="$SWAP_PID" DEPLOY_UNIT="$UNIT" DEPLOY_BIN="$label" \
			TAP_DIR="$TAP_DIR" timeout "$HEALTH_CMD_TIMEOUT" bash -c "$cmd"; then
			log "health: ok: $cmd"
		else
			gate_fail "health: $cmd"
		fi
	done

	if ! alive; then
		gate_fail "process: $UNIT is $U_STATE after the checks"
	fi
	((${#GATE_NOTES[@]} == 0))
}

# stops the unit, starts it on BIN with the saved arguments, runs the gates
swap_to() {
	local bin=$1
	stop_unit
	start_unit "$bin" || {
		GATE_NOTES=("start: systemd-run failed for $bin")
		return 1
	}
	gates "$bin"
}

# ---------------------------------------------------------------- links

set_link() {
	local name=$1 target=$2
	ln -sfn "$target" "$BIN_DIR/.$name.tmp"
	mv -Tf "$BIN_DIR/.$name.tmp" "$BIN_DIR/$name"
	log "$BIN_DIR/$name -> $target"
}

# ---------------------------------------------------------------- deploy

# the part after the build: BIN is BIN_DIR/transmog-<sha>, already there
deploy_bin() {
	local bin=$1 name
	name=$(basename "$bin")

	if [[ -n $PAUSE_FILE && -e $PAUSE_FILE ]]; then
		log "waiting for $PAUSE_FILE to go, up to ${PAUSE_TIMEOUT}s"
		local deadline=$((SECONDS + PAUSE_TIMEOUT))
		while [[ -e $PAUSE_FILE ]]; do
			((SECONDS < deadline)) || refuse "$PAUSE_FILE still there after ${PAUSE_TIMEOUT}s"
			sleep 1
		done
		log "$PAUSE_FILE gone"
	fi

	read_spec
	log "unit $UNIT runs $SPEC_BIN ${SPEC_ARGV[*]} (from $SPEC_FROM)"
	local prev=$SPEC_BIN
	if [[ $prev == "$bin" && -L $BIN_DIR/previous ]]; then
		prev=$(readlink "$BIN_DIR/previous")
	fi
	[[ -x $prev ]] || refuse "no previous build to fall back to: $prev"

	summary "deploying $name on $UNIT, fallback $(basename "$prev")"
	set_link previous "$prev"
	set_link current "$bin"
	if swap_to "$bin"; then
		save_spec_for "$bin"
		summary "deployed $name on $UNIT, pid $SWAP_PID: gates passed"
		return 0
	fi

	local why
	why=$(printf '%s; ' "${GATE_NOTES[@]}")
	summary "$name failed: ${why%; }; rolling back to $(basename "$prev")"
	set_link failed "$bin"
	set_link current "$prev"
	if swap_to "$prev"; then
		save_spec_for "$prev"
		summary "rolled back $UNIT to $(basename "$prev"), pid $SWAP_PID"
		return 1
	fi
	why=$(printf '%s; ' "${GATE_NOTES[@]}")
	summary "rollback to $(basename "$prev") failed too: ${why%; }"
	return 2
}

save_spec_for() {
	SPEC_BIN=$1
	save_spec
}

WORKTREE_PARENT=
cleanup() {
	if [[ -n $WORKTREE_PARENT && -d $WORKTREE_PARENT/src ]]; then
		git -C "$REPO" worktree remove --force "$WORKTREE_PARENT/src" >/dev/null 2>&1 || true
	fi
	if [[ -n $WORKTREE_PARENT && -d $WORKTREE_PARENT ]]; then
		rmdir "$WORKTREE_PARENT" 2>/dev/null || true
	fi
}

main() {
	local dry=no local_ci=no commit=''
	while (($# > 0)); do
		case $1 in
		--dry-run) dry=yes ;;
		--local-ci) local_ci=yes ;;
		-h | --help)
			usage
			exit 0
			;;
		-*)
			usage >&2
			exit 3
			;;
		*)
			[[ -z $commit ]] || {
				usage >&2
				exit 3
			}
			commit=$1
			;;
		esac
		shift
	done
	[[ -n $commit ]] || {
		usage >&2
		exit 3
	}

	load_env
	REPO=${REPO:-$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)}
	exec 9>"$STATE_DIR/deploy.lock"
	flock -n 9 || refuse "another deploy holds $STATE_DIR/deploy.lock"
	trap cleanup EXIT

	local mode=''
	[[ $dry == no ]] || mode+=' --dry-run'
	[[ $local_ci == no ]] || mode+=' --local-ci'
	log "---- deploy$mode $commit"
	local sha short
	sha=$(git -C "$REPO" rev-parse --verify --quiet "$commit^{commit}") ||
		refuse "$commit is not a commit in $REPO"
	short=$(git -C "$REPO" rev-parse --short=12 "$sha")
	run git -C "$REPO" fetch --quiet origin "$CI_BRANCH" ||
		log "fetch of origin/$CI_BRANCH failed, using the local ref"
	git -C "$REPO" merge-base --is-ancestor "$sha" "origin/$CI_BRANCH" ||
		refuse "$short is not reachable from origin/$CI_BRANCH"
	log "$short is on origin/$CI_BRANCH"

	WORKTREE_PARENT=$(mktemp -d "${TMPDIR:-/tmp}/transmog-deploy.XXXXXX")
	local wt=$WORKTREE_PARENT/src
	run git -C "$REPO" worktree add --detach "$wt" "$sha" ||
		refuse "could not check out $short"

	if [[ $local_ci == yes ]]; then
		run nix flake check "${NIX_ARGS[@]}" "$wt" || refuse "nix flake check failed at $short"
		log "ci: nix flake check passed at $short"
	else
		local runs
		runs=$(cd "$REPO" && gh run list --commit "$sha" --branch "$CI_BRANCH" \
			--json status,conclusion,workflowName) || refuse "gh run list failed"
		jq -e 'length > 0' >/dev/null <<<"$runs" ||
			refuse "no ci runs for $short on $CI_BRANCH"
		jq -e 'all(.status == "completed")' >/dev/null <<<"$runs" ||
			refuse "ci for $short is still running"
		jq -e 'all(.conclusion == "success")' >/dev/null <<<"$runs" ||
			refuse "ci for $short did not pass: $(jq -c '[.[] | {workflowName, conclusion}]' <<<"$runs")"
		log "ci: $(jq length <<<"$runs") run(s) passed for $short"
	fi

	mkdir -p "$STATE_DIR/gcroots"
	local root=$STATE_DIR/gcroots/transmog-$short out
	run nix build "${NIX_ARGS[@]}" "$wt#transmog" --out-link "$root" || refuse "nix build failed at $short"
	out=$(readlink -f "$root")
	[[ -x $out/bin/transmog ]] || refuse "$out has no bin/transmog"
	log "built $out, gc root $root"

	local bin=$BIN_DIR/transmog-$short
	set_link "transmog-$short" "$out/bin/transmog"

	if [[ $dry == yes ]]; then
		set_link staged "$bin"
		read_spec
		print_plan "$bin"
		summary "staged $(basename "$bin") for $UNIT (dry run)"
		return 0
	fi
	# a plain call, so set -e passes deploy_bin's 1 or 2 on as the exit status
	deploy_bin "$bin"
}

print_plan() {
	local bin=$1 p props=() state=absent
	for p in "${RUN_PROPS[@]}"; do props+=(-p "$p"); done
	unit_status
	[[ -z $PAUSE_FILE || ! -e $PAUSE_FILE ]] || state=present
	cat <<EOF
plan for $UNIT
  build      $(readlink -f "$bin")
  staged     $BIN_DIR/staged -> $bin
  now runs   $SPEC_BIN (unit $U_STATE, pid $U_PID)
  fallback   $(if [[ $SPEC_BIN == "$bin" ]]; then readlink "$BIN_DIR/previous" 2>/dev/null || echo none; else echo "$SPEC_BIN"; fi)
  arguments  from $SPEC_FROM
  pause file ${PAUSE_FILE:-unset} ($state)
  steps      wait for the pause file, stop $UNIT, wait up to ${PORT_WAIT_SECS}s for tcp ${NDI_PORTS:-none} to be free,
             then: $(printf '%q ' systemd-run --user --unit "$UNIT" "${props[@]}" "$bin" "${SPEC_ARGV[@]}")
  gates      over ${HEALTH_SECS}s: process alive; tap $TAP_DIR >= $MIN_FPS fps, <= $MAX_SEQ_GAPS seq gaps;
             tcp ${NDI_PORTS:-none} held by the new pid; control.state ${CONTROL_STATE:-not checked};
             ${#HEALTH_CMDS[@]} health command(s); then restart ${RESTART_UNITS[*]:-nothing}
  on failure $BIN_DIR/current -> fallback, same gates
EOF
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
	main "$@"
fi
