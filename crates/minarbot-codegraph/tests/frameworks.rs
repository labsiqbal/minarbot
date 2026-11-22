#![cfg(all(feature = "javascript", feature = "typescript", feature = "python"))]

use minarbot_codegraph::Index;
use rusqlite::Connection;
use std::{error::Error, fs};

#[test]
fn routes_link_to_handlers_and_navigation_refreshes_after_edit() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    for folder in ["app/(site)/people/[person]", "app/api/status", "web"] {
        fs::create_dir_all(root.join(folder))?;
    }
    for (file, text) in [
        ("package.json", r#"{"dependencies":{"express":"4","next":"15","react-router":"7","@nestjs/core":"11"}}"#),
        ("requirements.txt", "Django\nFlask\n"),
        ("app/(site)/people/[person]/page.tsx", "export default function Person() { return <p>Person</p>; }\n"),
        ("app/api/status/route.ts", "export function GET() { return Response.json({ok: true}); }\n"),
        ("web/links.tsx", r#"import Link from 'next/link'; export function Nav() { return <Link href="/people/alice?tab=1">Alice</Link>; }"#),
        ("web/router.tsx", r#"function Preferences() { return <div/>; } export const routes = <Route path="/prefs" element={<Preferences/>}/>;"#),
        ("web/main.ts", "import { NestFactory } from '@nestjs/core';\nconst app = NestFactory.create(AppModule);\napp.setGlobalPrefix('v1');\n"),
        ("web/users.controller.ts", "import { Controller, Get } from '@nestjs/common';\n@Controller('users')\nclass UsersController {\n  @Get('active')\n  list() { return []; }\n}\n"),
        ("web/api.js", "const express = require('express');\nconst app = express();\nfunction hello() {}\nfunction logAccess() {}\napp.get('/hello', hello);\napp.get('/inline', (req, res) => { logAccess(); });\n// app.get('/comment', hello);\n"),
        ("urls.py", "from django.urls import path\n\ndef dashboard(request):\n    return None\n\nurlpatterns = [path('dashboard/', dashboard)]\n"),
        ("web.py", "from flask import Flask\napp = Flask(__name__)\n@app.route('/items', methods=['GET', 'POST'])\ndef items():\n    return []\n"),
    ] {
        fs::write(root.join(file), text)?;
    }
    let database = scratch.path().join("graph.db");
    let mut index = Index::open(&root, &database)?;
    index.sync()?;
    let db = Connection::open(database)?;
    for (source, kind, target) in [
        ("Nav", "navigates", "/people/:person"),
        ("/people/:person", "calls", "Person"),
        ("GET /api/status", "references", "GET"),
        ("/prefs", "references", "Preferences"),
        ("GET /v1/users/active", "references", "list"),
        ("GET /hello", "references", "hello"),
        ("GET /inline", "calls", "logAccess"),
        ("dashboard/", "references", "dashboard"),
        ("POST /items", "references", "items"),
    ] {
        let found: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM edges e JOIN nodes a ON a.id=e.source
                JOIN nodes b ON b.id=e.target WHERE a.name=? AND e.kind=? AND b.name=?)",
            [source, kind, target],
            |row| row.get(0),
        )?;
        assert!(found, "{source} {kind} {target}");
    }

    fs::remove_file(root.join("app/(site)/people/[person]/page.tsx"))?;
    fs::write(root.join("web/api.js"), "const app = express();\n")?;
    index.sync()?;
    let stale: usize = db.query_row(
        "SELECT COUNT(*) FROM nodes WHERE kind='route' AND name IN ('/people/:person','GET /hello','GET /comment')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stale, 0);
    Ok(())
}
