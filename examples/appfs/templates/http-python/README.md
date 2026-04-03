# AppFS HTTP Connector Template (Python)

This directory is the scaffold source used by `examples/appfs/new-connector.sh`.

It mirrors the current Python HTTP connector bridge layout:

1. `bridge_server.py`
2. `appfs_http_bridge/`
3. `tests/`

The generated connector should implement the canonical `AppConnector` behavior surface and run under the managed AppFS runtime.
