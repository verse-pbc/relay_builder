#!/bin/bash

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "Setting up git hooks for nostr_relay_builder..."

# Create the pre-commit hook
cat > .git/hooks/pre-commit << 'EOF'
#!/bin/bash

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${YELLOW}Running pre-commit checks...${NC}"

# Check formatting
echo -e "${YELLOW}Checking code formatting...${NC}"
if ! cargo fmt -- --check; then
    echo -e "${RED}❌ Code formatting check failed!${NC}"
    echo -e "${YELLOW}Run 'cargo fmt' to fix formatting issues.${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Code formatting check passed${NC}"

# Run clippy
echo -e "${YELLOW}Running clippy...${NC}"
if ! cargo clippy --all-targets --all-features -- -D warnings; then
    echo -e "${RED}❌ Clippy check failed!${NC}"
    echo -e "${YELLOW}Fix the clippy warnings before committing.${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Clippy check passed${NC}"

# Optional: Run tests (uncomment if you want tests to run on every commit)
# echo -e "${YELLOW}Running tests...${NC}"
# if ! cargo test --all-features; then
#     echo -e "${RED}❌ Tests failed!${NC}"
#     exit 1
# fi
# echo -e "${GREEN}✓ All tests passed${NC}"

echo -e "${GREEN}✓ All pre-commit checks passed!${NC}"
EOF

# Make the hook executable
chmod +x .git/hooks/pre-commit

echo -e "${GREEN}✓ Git hooks setup complete!${NC}"
echo -e "${YELLOW}Pre-commit hook will run:${NC}"
echo "  - cargo fmt -- --check"
echo "  - cargo clippy --all-targets --all-features -- -D warnings"
echo ""
echo -e "${YELLOW}To run tests on every commit, uncomment the test section in .git/hooks/pre-commit${NC}"