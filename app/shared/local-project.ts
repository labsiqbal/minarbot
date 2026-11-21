export type LocalProject = {
  id: string;
  name: string;
  description: string;
  archived: boolean;
  allowInstallationApps: boolean;
  codegraphEnabled?: boolean;
  revision: number;
  updated: number;
  folders: string[];
  members: {
    agent: string;
    write: boolean;
  }[];
  channel: string;
  sections: {
    id: string;
    name: string;
    position: number;
    archived: boolean;
    revision: number;
    activity: number;
    read_position: number;
  }[];
  threads: {
    id: string;
    agent: string;
    name: string;
    folder: string;
  }[];
};
