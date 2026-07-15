// Recognises the auth/access failures the backend emits (auth.rs, kudu.rs) and
// turns them into a structured, actionable notice. Matching is on the stable
// leading phrase of each backend message, not the whole English sentence.

export interface AuthNotice {
  title: string;
  detail: string;
  /** Shell commands to run, shown in order with a copy button each. */
  commands: string[];
  /** Optional external link (e.g. the Azure CLI install page). */
  link?: { href: string; label: string };
}

export function parseAuthError(raw: string): AuthNotice | null {
  if (raw.includes('Azure CLI (az) not found')) {
    return {
      title: 'Azure CLI is not installed',
      detail:
        'LogLooker signs in through the Azure CLI. Install it, then run az login and retry.',
      commands: ['az login'],
      link: { href: 'https://aka.ms/azure-cli', label: 'Install Azure CLI' },
    };
  }

  if (raw.includes('Not signed in to Azure')) {
    return {
      title: 'Not signed in to Azure',
      detail:
        'Run az login in a terminal, complete the browser sign-in, then retry.',
      commands: ['az login'],
    };
  }

  if (raw.includes('No active Azure subscription')) {
    return {
      title: 'No active Azure subscription',
      detail:
        'Pick the subscription that holds these App Services, then retry.',
      commands: ['az account list -o table', 'az account set --subscription <name-or-id>'],
    };
  }

  if (raw.includes('Access denied (')) {
    return {
      title: 'Access denied to this app',
      detail:
        'Your Azure account may lack access to this App Service, or you are signed in to the wrong tenant or subscription. Check who you are, then switch tenant or subscription if needed and retry.',
      commands: [
        'az account show',
        'az login --tenant <tenant-id>',
        'az account set --subscription <name-or-id>',
      ],
    };
  }

  return null;
}
