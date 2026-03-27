# AI SDK + just-bash Code Explorer Agent

An interactive AI agent that combines [Vercel AI SDK](https://sdk.vercel.ai/), [just-bash](https://github.com/vercel-labs/just-bash) for bash command execution, and [AgentFS](https://github.com/esp3j0/appfs) for persistent filesystem storage.

This example is forked from the [just-bash bash-agent example](https://github.com/vercel-labs/just-bash/tree/main/examples/bash-agent) with AgentFS integration added.

## How It Works

- **Vercel AI SDK** - Orchestrates the AI agent with Claude as the model
- **just-bash** - Provides a bash tool that the AI can use to execute shell commands
- **AgentFS** - Backs the virtual filesystem with SQLite for persistence across sessions

## Files

- `main.ts` - Entry point
- `agent.ts` - Agent logic using AI SDK's `streamText` with just-bash's `createBashTool`
- `shell.ts` - Interactive readline shell

## Setup

1. Install dependencies:

   ```bash
   npm install
   ```

2. Set your Anthropic API key:

   ```bash
   export ANTHROPIC_API_KEY=your-key-here
   ```

3. Run:
   ```bash
   npm start
   ```

## Usage

Ask questions like:

- "What commands are available?"
- "How is the grep command implemented?"
- "Show me the Bash class"
- "Find all test files"

Type `exit` to quit.

## Development

```bash
npm run typecheck
```
