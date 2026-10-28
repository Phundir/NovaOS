#!/bin/sh
# Writes the reference outputs of the Python corpus with CPython 3.13:
# for each tests/python/NAME.py, NAME.out holds its standard output and,
# when it fails, "--- exit N" and the last line of its error output (the
# uncaught exception). `make py-test` compares Nova's interpreter with them.
# Usage: tools/pyhost/reference.sh [test.py...]
set -e
PYTHON=${PYTHON:-python3}
cd "$(dirname "$0")/../../tests/python"
if [ $# -eq 0 ]; then set -- *.py; fi
for test in "$@"; do
    name=$(basename "$test" .py)
    case "$name" in _*) continue ;; esac
    input=/dev/null
    [ -f "$name.in" ] && input="$name.in"
    set +e
    PYTHONHASHSEED=0 "$PYTHON" -X int_max_str_digits=4300 "$name.py" < "$input" > "$name.out" 2> "$name.err"
    code=$?
    set -e
    if [ $code -ne 0 ] || [ -s "$name.err" ]; then
        printf -- '--- exit %s\n' "$code" >> "$name.out"
        grep -v '^[[:space:]]*$' "$name.err" | tail -n 1 >> "$name.out"
    fi
    rm -f "$name.err"
done
