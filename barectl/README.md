# barectl

`barectl` is the command-line client for a Barenetes cluster. Its command
layout follows `kubectl`: a verb is followed by a resource type, and common
resource aliases are accepted.

## Usage

List resources:

```console
barectl get pod
barectl get pod web
barectl get node
barectl get node worker-1
```

Plural names and the aliases `po` for pods and `no` for nodes are supported:

```console
barectl get pods
barectl get po
barectl get nodes
barectl get no worker-1
```

Create a pod from a YAML manifest:

```console
barectl create pod -f barectl/examples/pod.yaml
```

Create a single-container pod directly from an image:

```console
barectl create pod web --image nginx:alpine
barectl create pod web --image nginx:alpine --env MODE=production --port 8080:80/tcp
```

Delete a pod:

```console
barectl delete pod web
barectl delete po web --namespace staging
```

`-n` is shorthand for `--namespace` wherever a pod namespace is accepted.

## API server

The client connects to `http://127.0.0.1:50052` by default. Select another API
server with the global `-s, --server` option or the `BARENETES_SERVER`
environment variable:

```console
barectl --server http://192.168.1.10:50052 get node
BARENETES_SERVER=http://192.168.1.10:50052 barectl get pod
```

The global option may also be placed after a subcommand.

## Shell completion

Generate completion code with `barectl completion <SHELL>`. Bash, Elvish, Fish,
PowerShell, and Zsh are supported.

Load completions for the current shell session:

```bash
source <(barectl completion bash)
```

```zsh
source <(barectl completion zsh)
```

```fish
barectl completion fish | source
```

```powershell
barectl completion powershell | Out-String | Invoke-Expression
```

For permanent completion, redirect the generated script to the completion
directory used by your shell. Release packages install Bash, Zsh, and Fish
completions automatically. For example, with Fish and a standalone binary:

```fish
barectl completion fish > ~/.config/fish/completions/barectl.fish
```

Run `barectl --help` or `barectl <command> --help` for the complete option list.
