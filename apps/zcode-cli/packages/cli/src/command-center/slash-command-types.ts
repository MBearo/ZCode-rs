export type SlashCommand =
  | {
      args: string;
      name: "compact";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "effort";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "expert";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "fork";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "help";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "init";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "workflow";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "locale";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "login";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "logout";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "mcp";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "plugins";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "mode";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "model";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "new";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "resume";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "rewind";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "skill";
      rawName: string;
      skillName: string;
      task: string;
      type: "known";
    }
  | {
      args: string;
      name: "goal";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      name: "dwf";
      rawName: string;
      type: "known";
    }
  | {
      args: string;
      rawName: string;
      type: "unknown";
    };
