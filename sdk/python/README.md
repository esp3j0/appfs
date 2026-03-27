# AgentFS Python SDK

A filesystem and key-value store for AI agents, powered by SQLite and [pyturso](https://pypi.org/project/pyturso/).

## Installation

```bash
pip install agentfs-sdk
```

## Quick Start

```python
import asyncio
from agentfs_sdk import AgentFS, AgentFSOptions

async def main():
    # Open an agent filesystem
    agent = await AgentFS.open(AgentFSOptions(id='my-agent'))

    # Use key-value store
    await agent.kv.set('config', {'debug': True, 'version': '1.0'})
    config = await agent.kv.get('config')
    print(f"Config: {config}")

    # Use filesystem
    await agent.fs.write_file('/data/notes.txt', 'Hello, AgentFS!')
    content = await agent.fs.read_file('/data/notes.txt')
    print(f"Content: {content}")

    # Track tool calls
    call_id = await agent.tools.start('search', {'query': 'Python'})
    await agent.tools.success(call_id, {'results': ['result1', 'result2']})

    # Get statistics
    stats = await agent.tools.get_stats()
    for stat in stats:
        print(f"{stat.name}: {stat.total_calls} calls, {stat.avg_duration_ms:.2f}ms avg")

    # Close the database
    await agent.close()

if __name__ == '__main__':
    asyncio.run(main())
```

## Features

### Key-Value Store

Simple key-value storage with JSON serialization:

```python
# Set a value
await agent.kv.set('user:123', {'name': 'Alice', 'age': 30})

# Get a value
user = await agent.kv.get('user:123')

# List by prefix
users = await agent.kv.list('user:')

# Delete a value
await agent.kv.delete('user:123')
```

### Filesystem

POSIX-like filesystem operations:

```python
# Write a file (creates parent directories automatically)
await agent.fs.write_file('/data/config.json', '{"key": "value"}')

# Read a file
content = await agent.fs.read_file('/data/config.json')

# Read as bytes
data = await agent.fs.read_file('/data/image.png', encoding=None)

# List directory
entries = await agent.fs.readdir('/data')

# Get file stats
stats = await agent.fs.stat('/data/config.json')
print(f"Size: {stats.size} bytes")
print(f"Modified: {stats.mtime}")
print(f"Is file: {stats.is_file()}")

# Delete a file
await agent.fs.delete_file('/data/config.json')
```

### Tool Calls Tracking

Track and analyze tool/function calls:

```python
# Start a tool call
call_id = await agent.tools.start('search', {'query': 'Python'})

# Mark as successful
await agent.tools.success(call_id, {'results': [...]})

# Or mark as failed
await agent.tools.error(call_id, 'Connection timeout')

# Record a completed call
await agent.tools.record(
    'search',
    started_at=1234567890,
    completed_at=1234567892,
    parameters={'query': 'Python'},
    result={'results': [...]}
)

# Query tool calls
calls = await agent.tools.get_by_name('search', limit=10)
recent = await agent.tools.get_recent(since=1234567890)

# Get statistics
stats = await agent.tools.get_stats()
for stat in stats:
    print(f"{stat.name}: {stat.successful}/{stat.total_calls} successful")
```

## Configuration

### Using Agent ID

Creates a database at `.agentfs/{id}.db`:

```python
agent = await AgentFS.open(AgentFSOptions(id='my-agent'))
```

### Using Custom Path

Specify a custom database path:

```python
agent = await AgentFS.open(AgentFSOptions(path='./data/mydb.db'))
```

### Using Both

You can specify both for clarity:

```python
agent = await AgentFS.open(AgentFSOptions(id='my-agent', path='./data/mydb.db'))
```

## Context Manager Support

Use AgentFS with async context managers:

```python
async with await AgentFS.open(AgentFSOptions(id='my-agent')) as agent:
    await agent.kv.set('key', 'value')
    # Database is automatically closed when exiting the context
```

## Development

### Setup

```bash
# Install dependencies
uv sync --group dev

# Run tests
uv run pytest

# Format code
uv run ruff format agentfs_sdk tests

# Check code
uv run ruff check agentfs_sdk tests
```

## License

MIT License - see LICENSE file for details.

## Links

- [GitHub Repository](https://github.com/esp3j0/appfs)
- [TypeScript SDK](https://github.com/esp3j0/appfs/tree/main/sdk/typescript)
- [tursodb](https://github.com/tursodatabase/turso)
- [pyturso](https://pypi.org/project/pyturso/)
