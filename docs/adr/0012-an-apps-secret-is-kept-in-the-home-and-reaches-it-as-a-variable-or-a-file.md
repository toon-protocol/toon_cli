# An app's secret is kept in the home and reaches it as a variable or a file

An app that calls a chain or signs anything needs a key, and an app was started with nothing but its port and its data directory. So an operator can give an app variables and secrets when adding it. A variable is a name and a value kept in the agent node's configuration, where `toon status` may show it. A secret is copied from a file of the operator's into the agent node's home, in a file only the operator's account can read, outside the directory the app writes to. Its value is in no configuration, output, log or error message of `toon`.

The supervisor starts and restarts apps and has no passphrase, so a secret sealed in the keystore could not reach an app that restarts unattended. That is the reasoning of ADR 0008, and the same place is used. We chose a copy over recording the path of the operator's file, which would let a secret be changed by editing that file but leaves the home short of what the agent node needs to start.

A secret reaches the app in one of two forms, chosen per secret: as a variable, or as a file mounted read-only at a path in the container. We chose both over the mounted file alone, which is the form that `docker inspect` does not show, because an image that reads a key from a variable cannot be given it any other way and an operator does not always control the image.

A variable or a secret is not an allowance (ADR 0011). An allowance loosens what an app can reach or use; these tell it something and loosen nothing. They share the allowance's lifecycle: set when the app is added, and changed afterwards by the operator without removing the app. A change names only what it gives or takes away, and everything else about the app stays as it was.

## Consequences

- Whoever can read the agent node's home can read every app's secrets, as they can already sign as the agent (ADR 0008).
- A secret passed as a variable can be read by anyone who can use docker on the machine. The mounted form cannot be read that way.
- Only the operator supplies a secret. An add from an agent key that carries a secret's contents is refused, and one that carries a variable is held as a pending app even for a trusted image (ADR 0010), since a trusted image is trusted as shipped and not under any configuration an agent chooses.
- An agent key may name a secret an app needs, by the variable's name or the path in the container, and give nothing for it. Such an add is always held as a pending app, and the operator gives a file for each secret named when approving it. The approval is refused unless it gives exactly the secrets named, no fewer and no more, so what the operator was shown is what runs. We chose this over letting the operator add or leave out a secret at approval, which would make the approved app something neither side asked for; an operator who wants it otherwise refuses it and adds the app themselves, or changes it afterwards.
- Replacing a leaked secret is a change to the app, which restarts the app, and its connector only if the app comes back somewhere else (ADR 0011). The copy it replaces is kept until the app has started with the new one, so a change that fails leaves the app running as it was.
- A name is one thing an app is started with. A secret given under the name of a variable replaces the variable, and the other way round.
- Only the operator changes an app. With an agent key a change is refused whatever it carries: each one loosens an app's confinement or reconfigures an image the operator approved as it was.
- A backup holds the agent node's keys and nothing about its apps, so variables and secrets are not in it. After a restore the operator adds each app again from their own files.
- Names beginning `TOON_` belong to the contract between `toon` and an app, and an operator cannot set one.
- The relay takes no variable or secret from the operator. What it is started with comes from its settings and the agent identity.
