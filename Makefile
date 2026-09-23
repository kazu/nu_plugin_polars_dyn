# `git_task ci` が叩く入口。ゲートの定義は toolkit.nu にあり、ここはそれを step ごとに
# 呼んで CI_LOG に追記するだけ。契約は docs.dev/workflow.md「`make ci` の契約」。

CI_LOG ?= /dev/null

# git_task ci が `make -q __git_task_ci_probe` で「この repo は make ci を提供している」と
# 確かめるための target。中身は要らない。
.PHONY: __git_task_ci_probe
__git_task_ci_probe: ; @true

.PHONY: ci
ci:
	@set +e; \
	  LOG="$(CI_LOG)"; MAX=0; SEP=0; \
	  run() { \
	    name="$$1"; shift; \
	    f="$$(mktemp)"; \
	    "$$@" >"$$f" 2>&1; e=$$?; \
	    { [ "$$SEP" = 1 ] && printf '\n'; printf '===== %s (exit %d) =====\n' "$$name" "$$e"; cat "$$f"; } >> "$$LOG"; \
	    rm -f "$$f"; \
	    SEP=1; \
	    [ $$e -gt $$MAX ] && MAX=$$e; \
	    return 0; \
	  }; \
	  run "toolkit fmt --check" nu -c "use toolkit.nu; toolkit fmt --check"; \
	  run "toolkit clippy" nu -c "use toolkit.nu; toolkit clippy"; \
	  run "toolkit test" nu -c "use toolkit.nu; toolkit test"; \
	  [ -n "$(CI_MAX_FILE)" ] && printf '%d\n' "$$MAX" > "$(CI_MAX_FILE)"; \
	  exit $$MAX

# `make release version=0.2.0`: ci を通してから、version を上げる commit を main に置き、tag を
# 打って push する。手順は toolkit.nu の `release`。
.PHONY: release
release: ci
	@test -n "$(version)" || { echo 'usage: make release version=MAJOR.MINOR.PATCH' >&2; exit 2; }
	nu -c "use toolkit.nu; toolkit release $(version)"
