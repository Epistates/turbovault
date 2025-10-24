Scanned with [MCP Scanner](https://github.com/cisco-ai-defense/mcp-scanner)

To run the scanner and repeat results:
```
cargo install turbovault

mcp-scanner --analyzers yara --format detailed --stdio-command turbovault --stdio-arg="--profile" --stdio-arg="production"
```

Results:
```
=== MCP Scanner Detailed Results ===

Scan Target: stdio:turbovault --profile production

Tool 1: get_forward_links
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 2: export_broken_links
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 3: edit_note
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 4: read_note
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 5: delete_note
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 6: get_ofm_syntax_guide
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 7: query_metadata
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 8: search
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 9: get_isolated_clusters
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 10: batch_execute
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 11: get_vault_config
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 12: get_ofm_quick_ref
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 13: write_note
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 14: suggest_links
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 15: explain_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 16: create_from_template
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 17: get_template
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 18: set_active_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 19: get_related_notes
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 20: export_analysis_report
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 21: remove_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 22: list_vaults
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 23: get_backlinks
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 24: export_health_report
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 25: get_link_strength
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 26: recommend_related
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 27: get_dead_end_notes
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 28: get_ofm_examples
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 29: full_health_analysis
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 30: get_broken_links
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 31: find_notes_from_template
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 32: add_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 33: list_templates
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 34: export_vault_stats
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 35: get_active_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 36: get_centrality_ranking
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 37: quick_health_check
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 38: move_note
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 39: create_vault
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 40: get_hub_notes
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 41: get_vault_context
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 42: detect_cycles
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 43: get_metadata_value
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0

Tool 44: advanced_search
Status: completed
Safe: Yes
Analyzer Results:
  • yara_analyzer:
    - Severity: SAFE
    - Threat Summary: No threats detected
    - Threat Names: None
    - Total Findings: 0
```