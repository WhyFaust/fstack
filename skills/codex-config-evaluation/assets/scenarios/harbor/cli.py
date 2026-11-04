def ingest(config, input_path, writer):
    result = writer(config["vault_path"], input_path)
    return {"saved": result, "vault": str(vault_path)}
