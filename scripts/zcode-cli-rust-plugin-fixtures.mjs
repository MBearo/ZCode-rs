// Run through generate-zcode-cli-rust-fixtures.mjs. Official plugin definitions
// and default marketplaces for the Rust plugins crate, taken from the TS source.
import {
  DEFAULT_ENABLED_OFFICIAL_PLUGIN_IDS,
  OFFICIAL_NODE_REPL_HOST_PLUGIN_NAME,
  OFFICIAL_PLUGIN_DEFINITIONS,
} from "../apps/zcode-cli/packages/bootstrap/src/app/official-plugin-definitions.ts";
import { parseEntryStoreListing } from "../apps/zcode-cli/packages/adapters/src/plugins/marketplace.ts";
import {
  ZCODE_INLINE_PLUGIN_MARKETPLACE,
  ZCODE_OFFICIAL_PLUGIN_MARKETPLACE,
} from "../apps/zcode-cli/packages/contracts/src/plugins/index.ts";
import { DEFAULT_PLUGIN_MARKETPLACES } from "../packages/shared/src/plugin-marketplaces.ts";

export function pluginOfficialData() {
  return {
    marketplace: ZCODE_OFFICIAL_PLUGIN_MARKETPLACE,
    inlineMarketplace: ZCODE_INLINE_PLUGIN_MARKETPLACE,
    nodeReplHost: OFFICIAL_NODE_REPL_HOST_PLUGIN_NAME,
    defaultMarketplaces: DEFAULT_PLUGIN_MARKETPLACES,
    definitions: OFFICIAL_PLUGIN_DEFINITIONS.map((definition) => {
      const id = `${definition.name}@${ZCODE_OFFICIAL_PLUGIN_MARKETPLACE}`;
      const listing = definition.listing
        ? parseEntryStoreListing({ name: definition.name, ...definition.listing })
        : undefined;
      return {
        name: definition.name,
        version: definition.version,
        defaultEnabled: DEFAULT_ENABLED_OFFICIAL_PLUGIN_IDS.has(id),
        hostMcpServerNames: [...(definition.hostMcpServerNames ?? [])],
        ...(listing ? { listing } : {}),
      };
    }),
  };
}
