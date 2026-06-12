# Video Recording

Capture browser automation as video for debugging, documentation, or verification.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Basic Recording](#basic-recording)
- [Recording Commands](#recording-commands)
- [Use Cases](#use-cases)
- [Best Practices](#best-practices)
- [Output Format](#output-format)
- [Limitations](#limitations)

## Basic Recording

```bash
# Start recording
chrome-use record start ./demo.webm

# Perform actions
chrome-use open https://example.com
chrome-use snapshot -i
chrome-use click @e1
chrome-use fill @e2 "test input"

# Stop and save
chrome-use record stop
```

## Recording Commands

```bash
# Start recording to file
chrome-use record start ./output.webm

# Stop current recording
chrome-use record stop

# Restart with new file (stops current + starts new)
chrome-use record restart ./take2.webm
```

## Use Cases

### Debugging Failed Automation

```bash
#!/bin/bash
# Record automation for debugging

chrome-use record start ./debug-$(date +%Y%m%d-%H%M%S).webm

# Run your automation
chrome-use open https://app.example.com
chrome-use snapshot -i
chrome-use click @e1 || {
    echo "Click failed - check recording"
    chrome-use record stop
    exit 1
}

chrome-use record stop
```

### Documentation Generation

```bash
#!/bin/bash
# Record workflow for documentation

chrome-use record start ./docs/how-to-login.webm

chrome-use open https://app.example.com/login
chrome-use wait 1000  # Pause for visibility

chrome-use snapshot -i
chrome-use fill @e1 "demo@example.com"
chrome-use wait 500

chrome-use fill @e2 "password"
chrome-use wait 500

chrome-use click @e3
chrome-use wait --load networkidle
chrome-use wait 1000  # Show result

chrome-use record stop
```

### CI/CD Test Evidence

```bash
#!/bin/bash
# Record E2E test runs for CI artifacts

TEST_NAME="${1:-e2e-test}"
RECORDING_DIR="./test-recordings"
mkdir -p "$RECORDING_DIR"

chrome-use record start "$RECORDING_DIR/$TEST_NAME-$(date +%s).webm"

# Run test
if run_e2e_test; then
    echo "Test passed"
else
    echo "Test failed - recording saved"
fi

chrome-use record stop
```

## Best Practices

### 1. Add Pauses for Clarity

```bash
# Slow down for human viewing
chrome-use click @e1
chrome-use wait 500  # Let viewer see result
```

### 2. Use Descriptive Filenames

```bash
# Include context in filename
chrome-use record start ./recordings/login-flow-2024-01-15.webm
chrome-use record start ./recordings/checkout-test-run-42.webm
```

### 3. Handle Recording in Error Cases

```bash
#!/bin/bash
set -e

cleanup() {
    chrome-use record stop 2>/dev/null || true
    chrome-use close 2>/dev/null || true
}
trap cleanup EXIT

chrome-use record start ./automation.webm
# ... automation steps ...
```

### 4. Combine with Screenshots

```bash
# Record video AND capture key frames
chrome-use record start ./flow.webm

chrome-use open https://example.com
chrome-use screenshot ./screenshots/step1-homepage.png

chrome-use click @e1
chrome-use screenshot ./screenshots/step2-after-click.png

chrome-use record stop
```

## Output Format

- Default format: WebM (VP8/VP9 codec)
- Compatible with all modern browsers and video players
- Compressed but high quality

## Limitations

- Recording adds slight overhead to automation
- Large recordings can consume significant disk space
- Some headless environments may have codec limitations
