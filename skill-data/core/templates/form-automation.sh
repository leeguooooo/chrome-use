#!/bin/bash
# Template: Form Automation Workflow
# Purpose: Fill and submit web forms with validation
# Usage: ./form-automation.sh <form-url>
#
# This template demonstrates the snapshot-interact-verify pattern:
# 1. Navigate to form
# 2. Snapshot to get element refs
# 3. Fill fields using refs
# 4. Submit and verify result
#
# Customize: Update the refs (@e1, @e2, etc.) based on your form's snapshot output

set -euo pipefail

FORM_URL="${1:?Usage: $0 <form-url>}"

echo "Form automation: $FORM_URL"

# Step 1: Navigate to form
chrome-use open "$FORM_URL"
chrome-use wait --load networkidle

# Step 2: Snapshot to discover form elements
echo ""
echo "Form structure:"
chrome-use snapshot -i

# Step 3: Fill form fields (customize these refs based on snapshot output)
#
# Common field types:
#   chrome-use fill @e1 "John Doe"           # Text input
#   chrome-use fill @e2 "user@example.com"   # Email input
#   chrome-use fill @e3 "SecureP@ss123"      # Password input
#   chrome-use select @e4 "Option Value"     # Dropdown
#   chrome-use check @e5                     # Checkbox
#   chrome-use click @e6                     # Radio button
#   chrome-use fill @e7 "Multi-line text"   # Textarea
#   chrome-use upload @e8 /path/to/file.pdf # File upload
#
# Uncomment and modify:
# chrome-use fill @e1 "Test User"
# chrome-use fill @e2 "test@example.com"
# chrome-use click @e3  # Submit button

# Step 4: Wait for submission
# chrome-use wait --load networkidle
# chrome-use wait --url "**/success"  # Or wait for redirect

# Step 5: Verify result
echo ""
echo "Result:"
chrome-use get url
chrome-use snapshot -i

# Optional: Capture evidence
chrome-use screenshot /tmp/form-result.png
echo "Screenshot saved: /tmp/form-result.png"

# Cleanup
chrome-use close
echo "Done"
