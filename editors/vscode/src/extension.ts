import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from 'vscode-languageclient/node';

interface LspPosition {
    line: number;
    character: number;
}

interface LspTextEdit {
    range: {
        start: LspPosition;
        end: LspPosition;
    };
    newText: string;
}

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    const ksnPath = vscode.workspace
        .getConfiguration('kasane')
        .get<string>('ksnPath', 'ksn');
    const serverOptions: ServerOptions = {
        command: ksnPath,
        args: ['lsp'],
        transport: TransportKind.stdio,
    };
    const outputChannel = vscode.window.createOutputChannel(
        'Kasane Language Server',
    );
    context.subscriptions.push(outputChannel);
    const clientOptions: LanguageClientOptions = {
        documentSelector: [
            {
                language: 'kasane',
                scheme: 'file',
            },
        ],
        outputChannel,
    };

    const languageClient = new LanguageClient(
        'kasane',
        'Kasane Language Server',
        serverOptions,
        clientOptions,
    );
    client = languageClient;
    context.subscriptions.push(languageClient);
    await languageClient.start();

    // stage0/src/lsp.rs は documentFormattingProvider を返すため、
    // 保存時の整形は言語サーバに任せる。
    context.subscriptions.push(
        vscode.workspace.onWillSaveTextDocument((event) => {
            if (
                event.document.languageId !== 'kasane' ||
                event.document.uri.scheme !== 'file'
            ) {
                return;
            }
            event.waitUntil(
                (async () => {
                    try {
                        const editor = vscode.workspace.getConfiguration(
                            'editor',
                            event.document.uri,
                        );
                        const edits =
                            await languageClient.sendRequest<LspTextEdit[] | null>(
                                'textDocument/formatting',
                                {
                                    textDocument: {
                                        uri: event.document.uri.toString(),
                                    },
                                    options: {
                                        tabSize: editor.get<number>('tabSize', 4),
                                        insertSpaces: editor.get<boolean>(
                                            'insertSpaces',
                                            true,
                                        ),
                                    },
                                },
                            );
                        if (!edits?.length) {
                            return;
                        }
                        const edit = new vscode.WorkspaceEdit();
                        edit.set(
                            event.document.uri,
                            edits.map(
                                (item) =>
                                    new vscode.TextEdit(
                                        new vscode.Range(
                                            item.range.start.line,
                                            item.range.start.character,
                                            item.range.end.line,
                                            item.range.end.character,
                                        ),
                                        item.newText,
                                    ),
                            ),
                        );
                        const applied = await vscode.workspace.applyEdit(edit);
                        if (!applied) {
                            outputChannel.appendLine(
                                'Kasane: 保存時の整形を適用できませんでした',
                            );
                        }
                    } catch (error) {
                        outputChannel.appendLine(
                            `Kasane: 保存時の整形に失敗しました: ${String(error)}`,
                        );
                    }
                })(),
            );
        }),
    );
}

export async function deactivate(): Promise<void> {
    await client?.stop();
}
