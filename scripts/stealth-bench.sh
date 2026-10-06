#!/usr/bin/env bash
# Re-run the public bot detectors that docs/stealth.html quotes, against a real
# headed Chrome, and fail if any result regressed. Run it locally before a
# release that touches stealth, the relay or launch flags: it drives a real
# browser, so it cannot run in CI or on a remote build box.
#
#   scripts/stealth-bench.sh              # --launch and relay (your real Chrome)
#   scripts/stealth-bench.sh --launch     # isolated launched Chrome only
#   scripts/stealth-bench.sh --relay      # extension relay only
#   CHROME_USE=cli/target/debug/chrome-use scripts/stealth-bench.sh
#
# The detector sites change over time. When one stops testing anything (as
# nowsecure.nl did), replace it here and on docs/stealth.html together.
set -u
CU=${CHROME_USE:-chrome-use}
MODES=(launch relay)
case "${1:-}" in
  --launch) MODES=(launch) ;;
  --relay) MODES=(relay) ;;
  "") ;;
  *) echo "usage: $0 [--launch|--relay]" >&2; exit 2 ;;
esac

CF_URL=https://www.scrapingcourse.com/cloudflare-challenge
FAILS=0
pass() { printf '  \033[32mPASS\033[0m %-34s %s\n' "$1" "${2:-}"; }
fail() { printf '  \033[31mFAIL\033[0m %-34s %s\n' "$1" "${2:-}"; FAILS=$((FAILS + 1)); }

# cu <session> <mode> <args...>: one chrome-use call in the given mode.
cu() {
  local s=$1 m=$2; shift 2
  if [ "$m" = launch ]; then "$CU" --session "$s" --launch "$@" 2>/dev/null
  else "$CU" --session "$s" "$@" 2>/dev/null; fi
}
# ev <session> <mode> <js>: evaluate and print the result with JSON string quoting removed.
ev() { cu "$1" "$2" eval "$3" | tail -1 | sed -e 's/^"//' -e 's/"$//' -e 's/\\"/"/g'; }
# poll <session> <mode> <js> <seconds>: re-evaluate until the result is non-empty.
poll() {
  local out i
  for ((i = 0; i < $4; i += 2)); do
    out=$(ev "$1" "$2" "$3")
    [ -n "$out" ] && [ "$out" != null ] && { echo "$out"; return; }
    sleep 2
  done
}

WORKER_LANGS='(async()=>{const w=new Worker(URL.createObjectURL(new Blob(["postMessage(navigator.languages)"])));const r=await new Promise(res=>w.onmessage=e=>res(e.data));return JSON.stringify(navigator.languages)===JSON.stringify(r)?"same "+r:"main "+navigator.languages+" / worker "+r})()'

# Cloudflare's managed challenge: a real browser passes without interaction.
# Cloudflare grows stricter and less consistent toward an IP that has solved
# many challenges recently (a day of bisecting made even stealth-off runs spin),
# so a launched browser gets up to three fresh profiles before this fails.
# Treat "passed on attempt 3" as noise and repeated failures as a regression.
check_cloudflare() {
  local s=$1 m=$2 label=$3 t i attempt
  local attempts=1
  [ "$m" = launch ] && attempts=3
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    [ "$attempt" -gt 1 ] && cu "$s" "$m" close >/dev/null
    cu "$s" "$m" open "$CF_URL" >/dev/null
    for ((i = 8; i <= 64; i += 8)); do
      sleep 8
      t=$(cu "$s" "$m" get title | tail -1)
      if [[ "$t" == *"Cloudflare Challenge"* ]]; then
        pass "$label" "passed after ~${i}s (attempt $attempt/$attempts)"
        return
      fi
    done
  done
  fail "$label" "still on \"$t\" after 64s, $attempts attempt(s)"
}

check_detectors() {
  local s=$1 m=$2 out
  cu "$s" "$m" open https://bot.sannysoft.com >/dev/null
  out=$(poll "$s" "$m" 'document.querySelectorAll("td.passed").length>20?[...document.querySelectorAll("td.failed,td.warn")].map(td=>td.previousElementSibling?.innerText.trim()).join(", ")||"clean":""' 20)
  [ "$out" = clean ] && pass "sannysoft" "no failed/warn rows" || fail "sannysoft" "${out:-did not load}"

  cu "$s" "$m" open https://abrahamjuliot.github.io/creepjs/ >/dev/null
  out=$(poll "$s" "$m" '(()=>{const t=document.body.innerText;const h=t.match(/(\d+)% headless: [0-9a-f]{8}/),st=t.match(/(\d+)% stealth: [0-9a-f]{8}/);return h&&st?h[1]+" "+st[1]:""})()' 40)
  local headless=${out%% *} stealth=${out##* } max_stealth=0
  # The launch path's srcdoc-iframe proxy trips CreepJS hasIframeProxy (documented ~20%).
  [ "$m" = launch ] && [ "${AGENT_BROWSER_DISABLE_IFRAME_PROXY:-}" != 1 ] && max_stealth=20
  if [ -z "$out" ]; then fail "creepjs" "did not finish"
  elif [ "$headless" -eq 0 ] && [ "$stealth" -le "$max_stealth" ]; then pass "creepjs" "${headless}% headless, ${stealth}% stealth"
  else fail "creepjs" "${headless}% headless, ${stealth}% stealth (max ${max_stealth}%)"; fi

  cu "$s" "$m" open https://bot.incolumitas.com/ >/dev/null
  out=$(poll "$s" "$m" '(()=>{const e=document.getElementById("new-tests");if(!e||!/inconsistentWebWorker/.test(e.innerText))return "";const bad=e.innerText.split("\n").filter(l=>/FAIL|WARN/.test(l)).map(l=>l.trim());return bad.join(" ")||"clean"})()' 30)
  [ "$out" = clean ] && pass "incolumitas new-tests" "all OK" || fail "incolumitas new-tests" "${out:-did not load}"

  cu "$s" "$m" open https://www.browserscan.net/bot-detection >/dev/null
  sleep 10
  local text normal bad
  if text=$(cu "$s" "$m" get text) && normal=$(grep -cw Normal <<<"$text") && [ "$normal" -ge 10 ]; then
    bad=$(grep -cwE "Abnormal|Detected" <<<"$text")
    [ "$bad" = 0 ] && pass "browserscan" "$normal normal rows, none abnormal" || fail "browserscan" "$bad abnormal/detected rows"
  else
    fail "browserscan" "results did not load"
  fi

  out=$(ev "$s" "$m" "$WORKER_LANGS")
  [[ "$out" == same* ]] && pass "worker languages" "$out" || fail "worker languages" "$out"

  check_cloudflare "$s" "$m" "cloudflare managed challenge"
}

run_launch() {
  echo "== --launch (isolated Chrome)"
  check_detectors sbench-launch launch
  cu sbench-launch launch close >/dev/null

  echo "== --launch with AGENT_BROWSER_LOCALE=ja-JP"
  export AGENT_BROWSER_LOCALE=ja-JP
  local out
  cu sbench-locale launch open about:blank >/dev/null
  out=$(ev sbench-locale launch "$WORKER_LANGS")
  [ "$out" = "same ja-JP,ja" ] && pass "locale applied everywhere" "$out" || fail "locale applied everywhere" "$out"
  check_cloudflare sbench-locale launch "cloudflare with locale"
  cu sbench-locale launch close >/dev/null
  unset AGENT_BROWSER_LOCALE
}

run_relay() {
  echo "== relay (your real Chrome, zero JS patches)"
  if ! "$CU" stealth status 2>/dev/null | grep -q "mode: connect"; then
    fail "relay connection" "extension relay not connected; run: $CU extension connect"
    return
  fi
  check_detectors sbench-relay relay

  # An embedded Turnstile widget on an ordinary page is not a challenge page.
  cu sbench-relay relay open https://nowsecure.nl >/dev/null
  sleep 5
  if cu sbench-relay relay cf-status --json | grep -q '"challenged":false'; then
    pass "cf-status on embedded Turnstile" "not reported as a challenge"
  else
    fail "cf-status on embedded Turnstile" "reported as a challenge page"
  fi
  cu sbench-relay relay close >/dev/null
}

"$CU" --version | head -1
for m in "${MODES[@]}"; do "run_$m"; done
echo
if [ "$FAILS" -eq 0 ]; then echo "stealth bench: all checks passed"; else echo "stealth bench: $FAILS check(s) failed"; exit 1; fi
