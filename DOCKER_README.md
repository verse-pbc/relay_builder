# Docker Build Instructions

This project depends on `websocket_builder` which is a sibling directory. 
The Docker build MUST be run from the parent directory to include both projects.

## Build Options

### Option 1: Use the build script (RECOMMENDED)
From the `relay_builder` directory:
```bash
./docker-build.sh
```

### Option 2: Use docker-compose
From the `relay_builder` directory:
```bash
docker-compose up --build
```

### Option 3: Build manually
From the PARENT directory (`relay_repos`):
```bash
cd /Users/daniel/code/nos/groups/relay_repos
docker build -f relay_builder/Dockerfile -t relay-builder .
```

## Common Mistakes

❌ **WRONG** - Running from relay_builder directory:
```bash
cd relay_builder
docker build -t relay-builder .  # This will fail!
```

✅ **CORRECT** - Running from parent directory:
```bash
cd relay_repos  # Parent directory
docker build -f relay_builder/Dockerfile -t relay-builder .
```

## Why?

The Dockerfile needs to copy both `websocket_builder` and `relay_builder` directories.
Docker can only access files within its build context, so we must run from the parent
directory that contains both projects.