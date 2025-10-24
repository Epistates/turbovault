# Troubleshooting Guide

Common issues and solutions for TurboVault.

## Common Issues

### 1. Vault Not Found

**Error:**
```
Error: Failed to create vault config: Vault path does not exist: /path/to/vault
```

**Solution:**
- Verify the path is correct: `ls -la /path/to/vault`
- Use absolute paths (not relative or `~`)
- Ensure vault directory exists and contains `.obsidian/` folder
- Check file permissions

### 2. Server Starts But No Tools Available

**Symptoms:**
- Server runs without errors
- Claude doesn't see any tools
- MCP connection shows but no tools listed

**Solution:**
- Check server was initialized with vault:
  ```bash
  # Look for these log lines:
  [INFO] Server initialized with vault
  [INFO] Vault: /path/to/vault
  ```
- If missing, use `--vault` flag:
  ```bash
  mcp-obsidian --vault /path/to/vault --init
  ```
- Verify vault is valid (contains `.obsidian/` and `.md` files)

### 3. Permission Denied Errors

**Error:**
```
Error: Permission denied (os error 13)
```

**Solution:**
- Check vault directory permissions:
  ```bash
  ls -ld /path/to/vault
  # Should show read/write for your user
  ```
- Fix permissions:
  ```bash
  chmod -R u+rw /path/to/vault
  ```
- For Docker: ensure volume mount is correct and user has access

### 4. Claude Desktop Connection Issues

**Problem:** Claude doesn't see the server

**Solution:**
1. Check Claude Desktop logs:
   ```bash
   # macOS
   tail -f ~/Library/Logs/Claude/mcp*.log

   # Linux
   tail -f ~/.config/Claude/logs/mcp*.log
   ```

2. Verify server runs standalone:
   ```bash
   /path/to/mcp-obsidian --vault /path/to/vault --init
   ```

3. Check config file syntax (must be valid JSON)

4. Use absolute paths (not `~` or relative paths)

### 5. Search Returns No Results

**Symptoms:**
- `search()` tool returns empty array
- You know matching content exists

**Solution:**
- Ensure vault was initialized with `--init` flag
- Check search query syntax (use simple keywords first)
- Verify files have content (not just frontmatter)
- Check excluded paths (might be filtering out results)
- Review search index build in logs:
  ```
  [INFO] Building search index... (1250 files)
  [INFO] Search index ready
  ```

### 6. High Memory Usage

**Symptoms:**
- Server using excessive RAM (>500MB for <10k notes)
- System becomes sluggish

**Solution:**
- Use `high-performance` profile (disables file watching)
- Reduce cache TTL:
  ```bash
  # Shorten cache lifetime
  mcp-obsidian --vault /path/to/vault --profile production
  ```
- Check for memory leak (shouldn't happen, but report if found)
- Restart server periodically (systemd handles this)

### 7. Slow Performance

**Symptoms:**
- Operations take >5 seconds
- Search is slow (>1 second)

**Solution:**
- Use `--init` to build graph once on startup (not on-demand)
- Profile the slow operation:
  ```bash
  export RUST_LOG=debug
  # Look for slow operations in logs
  ```
- Check disk I/O (vault on slow HDD?)
- Reduce vault size or split into multiple vaults
- Disable file watching in `high-performance` profile

## Debug Mode

**Enable detailed logging:**

```bash
export RUST_LOG=debug
mcp-obsidian --vault /path/to/vault --init 2>&1 | tee debug.log
```

**Send debug logs when reporting issues:**

```bash
# Sanitize logs (remove sensitive paths)
sed 's|/Users/yourname/|/home/user/|g' debug.log > debug-sanitized.log
# Attach debug-sanitized.log to issue report
```

## Getting Help

1. **Check logs**: Look for ERROR or WARN messages
2. **Search issues**: https://github.com/epistates/TurboVault/issues
3. **Create issue**: Include:
   - Server version (`mcp-obsidian --version`)
   - OS and Rust version (`rustc --version`)
   - Vault size (number of files)
   - Minimal reproduction steps
   - Relevant log output (sanitized)

## FAQ

**Q: Do I need to install Obsidian to use this?**

A: No! TurboVault works with any Obsidian vault (just a folder of markdown files). Obsidian itself is optional.

**Q: Does this modify my vault?**

A: Only when you use write operations (create, delete, move). All modifications are atomic and logged. Use `--profile readonly` for safe exploration.

**Q: Can I use this with multiple vaults?**

A: Yes! Use `VaultLifecycleTools` to manage multiple vaults and switch between them. CLI support for multi-vault is coming soon.

**Q: What happens if the server crashes?**

A: All operations are atomic. Your vault will never be left in a broken state. Batch operations stop on first error with detailed reporting.

**Q: How do I back up my vault?**

A: TurboVault doesn't manage backups. Use git, rsync, or your preferred backup tool. The server only reads/writes markdown files.

**Q: Is this compatible with Obsidian Sync?**

A: Yes! TurboVault just reads/writes markdown files. Obsidian Sync will sync changes like normal.

**Q: Can I run this on a server?**

A: Yes! See the [systemd deployment](../deployment/) guide.

**Q: What's the difference between this and the Obsidian API?**

A: Obsidian API is JavaScript-based and requires Obsidian to be running. TurboVault is a standalone Rust server that works directly with vault files via MCP.

**Q: Does this support plugins?**

A: Not yet. The current version provides core vault operations and link graph analysis. Plugin support is planned for a future release.
