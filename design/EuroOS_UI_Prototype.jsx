import { useState, useEffect, useRef } from "react";

// ─── KLEURENPALET ────────────────────────────────────────────
const P = {
  bg:        "#0F1623",
  bgSurface: "#161E2E",
  bgCard:    "#1C2740",
  bgHover:   "#243152",
  border:    "#2A3A5C",
  borderLight:"#3A4F7A",
  blue:      "#2E7DD1",
  blueLight: "#4A9AE8",
  blueDark:  "#1A5AA8",
  accent:    "#38BDF8",
  accentDim: "#1E6A8A",
  green:     "#22C55E",
  yellow:    "#EAB308",
  red:       "#EF4444",
  textPrim:  "#F0F4FF",
  textSec:   "#8A9BB8",
  textDim:   "#4A5A78",
  white:     "#FFFFFF",
};

// ─── GEBRUIKERS DATA ─────────────────────────────────────────
const USERS = [
  { id: 1, name: "Jeroen",  role: "admin", avatar: "JV", color: "#2E7DD1", lastLogin: "Vandaag 08:12" },
  { id: 2, name: "Marie",   role: "user",  avatar: "MA", color: "#22C55E", lastLogin: "Gisteren 16:44" },
  { id: 3, name: "Thomas",  role: "user",  avatar: "TK", color: "#EAB308", lastLogin: "2 dagen geleden" },
  { id: 4, name: "Gast",    role: "guest", avatar: "G",  color: "#8A9BB8", lastLogin: "Nooit" },
];

const ROLE_LABELS = { admin: "Beheerder", user: "Gebruiker", guest: "Gast", root: "Systeem" };
const ROLE_COLORS = { admin: P.blue, user: P.green, guest: P.textDim, root: P.red };

// ─── OPEN APPS (demo) ────────────────────────────────────────
const APPS = [
  { id: "files",    label: "Bestanden",    icon: "📁", color: "#2E7DD1" },
  { id: "browser",  label: "Browser",      icon: "🌐", color: "#22C55E" },
  { id: "terminal", label: "Terminal",     icon: "⬛", color: "#4A5A78" },
  { id: "settings", label: "Instellingen", icon: "⚙️", color: "#8A9BB8" },
  { id: "editor",   label: "Editor",       icon: "📝", color: "#EAB308" },
];

// ─── HELPERS ─────────────────────────────────────────────────
function Avatar({ user, size = 36, ring = false }) {
  return (
    <div style={{
      width: size, height: size, borderRadius: "50%",
      background: user.color + "33",
      border: `2px solid ${ring ? user.color : P.border}`,
      display: "flex", alignItems: "center", justifyContent: "center",
      fontSize: size * 0.35, fontWeight: 700, color: user.color,
      fontFamily: "'DM Mono', monospace", flexShrink: 0,
      boxShadow: ring ? `0 0 0 3px ${user.color}22` : "none",
      transition: "all 0.2s",
    }}>
      {user.avatar}
    </div>
  );
}

function RoleBadge({ role }) {
  return (
    <span style={{
      fontSize: 10, fontWeight: 600, letterSpacing: "0.08em",
      color: ROLE_COLORS[role], background: ROLE_COLORS[role] + "18",
      border: `1px solid ${ROLE_COLORS[role]}33`,
      padding: "2px 7px", borderRadius: 4,
      fontFamily: "'DM Mono', monospace",
      textTransform: "uppercase",
    }}>
      {ROLE_LABELS[role]}
    </span>
  );
}

function Clock() {
  const [time, setTime] = useState(new Date());
  useEffect(() => {
    const t = setInterval(() => setTime(new Date()), 1000);
    return () => clearInterval(t);
  }, []);
  const pad = n => String(n).padStart(2, "0");
  const days = ["zo","ma","di","wo","do","vr","za"];
  const months = ["jan","feb","mrt","apr","mei","jun","jul","aug","sep","okt","nov","dec"];
  return (
    <div style={{ textAlign: "right", lineHeight: 1.3 }}>
      <div style={{ fontSize: 13, fontWeight: 700, color: P.textPrim, fontFamily: "'DM Mono', monospace" }}>
        {pad(time.getHours())}:{pad(time.getMinutes())}
      </div>
      <div style={{ fontSize: 10, color: P.textSec }}>
        {days[time.getDay()]} {time.getDate()} {months[time.getMonth()]}
      </div>
    </div>
  );
}

// ─── LOGIN SCHERM ─────────────────────────────────────────────
function LoginScreen({ onLogin }) {
  const [selected, setSelected] = useState(null);
  const [password, setPassword] = useState("");
  const [error, setError] = useState(false);
  const [shake, setShake] = useState(false);
  const inputRef = useRef();

  const handleSelect = (user) => {
    setSelected(user);
    setPassword("");
    setError(false);
    setTimeout(() => inputRef.current?.focus(), 100);
  };

  const handleLogin = () => {
    if (!selected) return;
    if (selected.role === "guest" || password.length > 0) {
      onLogin(selected);
    } else {
      setError(true);
      setShake(true);
      setTimeout(() => setShake(false), 500);
    }
  };

  return (
    <div style={{
      width: "100%", height: "100%",
      background: `radial-gradient(ellipse at 30% 40%, #1A3A6B44 0%, transparent 60%),
                   radial-gradient(ellipse at 70% 70%, #0D2A4A55 0%, transparent 50%),
                   ${P.bg}`,
      display: "flex", flexDirection: "column",
      alignItems: "center", justifyContent: "center",
      fontFamily: "'DM Sans', sans-serif",
      position: "relative", overflow: "hidden",
    }}>
      {/* Achtergrond raster patroon */}
      <div style={{
        position: "absolute", inset: 0, opacity: 0.04,
        backgroundImage: `linear-gradient(${P.blue} 1px, transparent 1px),
                          linear-gradient(90deg, ${P.blue} 1px, transparent 1px)`,
        backgroundSize: "40px 40px",
      }} />

      {/* Logo */}
      <div style={{ textAlign: "center", marginBottom: 48 }}>
        <div style={{
          fontSize: 11, letterSpacing: "0.3em", color: P.accent,
          fontFamily: "'DM Mono', monospace", marginBottom: 10,
          textTransform: "uppercase",
        }}>
          EuroKernel OS v0.1
        </div>
        <div style={{
          fontSize: 36, fontWeight: 800, color: P.textPrim,
          letterSpacing: "-0.02em", lineHeight: 1,
        }}>
          EURO<span style={{ color: P.accent }}>OS</span>
        </div>
        <div style={{ fontSize: 12, color: P.textSec, marginTop: 6 }}>
          Europees Soeverein Besturingssysteem
        </div>
      </div>

      {/* Gebruikers selectie */}
      {!selected ? (
        <div style={{ width: 420 }}>
          <div style={{ fontSize: 12, color: P.textSec, marginBottom: 14, textAlign: "center" }}>
            Selecteer een gebruiker
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {USERS.map(user => (
              <button key={user.id}
                onClick={() => handleSelect(user)}
                style={{
                  display: "flex", alignItems: "center", gap: 14,
                  background: P.bgSurface, border: `1px solid ${P.border}`,
                  borderRadius: 10, padding: "12px 16px", cursor: "pointer",
                  transition: "all 0.15s", textAlign: "left",
                  color: P.textPrim,
                }}
                onMouseEnter={e => {
                  e.currentTarget.style.background = P.bgHover;
                  e.currentTarget.style.borderColor = user.color + "66";
                }}
                onMouseLeave={e => {
                  e.currentTarget.style.background = P.bgSurface;
                  e.currentTarget.style.borderColor = P.border;
                }}
              >
                <Avatar user={user} size={42} />
                <div style={{ flex: 1 }}>
                  <div style={{ fontWeight: 600, fontSize: 15 }}>{user.name}</div>
                  <div style={{ fontSize: 11, color: P.textSec, marginTop: 2 }}>
                    Laatste login: {user.lastLogin}
                  </div>
                </div>
                <RoleBadge role={user.role} />
              </button>
            ))}
          </div>
        </div>
      ) : (
        /* Wachtwoord invoer */
        <div style={{
          width: 360, textAlign: "center",
          animation: shake ? "shake 0.4s ease" : "none",
        }}>
          <style>{`
            @keyframes shake {
              0%,100%{transform:translateX(0)}
              20%{transform:translateX(-8px)}
              40%{transform:translateX(8px)}
              60%{transform:translateX(-6px)}
              80%{transform:translateX(6px)}
            }
          `}</style>
          <Avatar user={selected} size={64} ring />
          <div style={{ fontSize: 18, fontWeight: 700, color: P.textPrim, marginTop: 12 }}>
            {selected.name}
          </div>
          <RoleBadge role={selected.role} />

          {selected.role !== "guest" ? (
            <div style={{ marginTop: 24 }}>
              <input
                ref={inputRef}
                type="password"
                placeholder="Wachtwoord"
                value={password}
                onChange={e => { setPassword(e.target.value); setError(false); }}
                onKeyDown={e => e.key === "Enter" && handleLogin()}
                style={{
                  width: "100%", padding: "12px 16px",
                  background: P.bgCard, border: `1px solid ${error ? P.red : P.border}`,
                  borderRadius: 8, color: P.textPrim, fontSize: 15,
                  outline: "none", boxSizing: "border-box",
                  fontFamily: "'DM Mono', monospace",
                  transition: "border-color 0.2s",
                }}
              />
              {error && (
                <div style={{ fontSize: 12, color: P.red, marginTop: 6 }}>
                  Ongeldig wachtwoord
                </div>
              )}
            </div>
          ) : (
            <div style={{ marginTop: 16, fontSize: 12, color: P.textSec }}>
              Gastmodus — geen wachtwoord vereist
            </div>
          )}

          <div style={{ display: "flex", gap: 10, marginTop: 20 }}>
            <button
              onClick={() => setSelected(null)}
              style={{
                flex: 1, padding: "10px", background: "transparent",
                border: `1px solid ${P.border}`, borderRadius: 8,
                color: P.textSec, cursor: "pointer", fontSize: 14,
                transition: "all 0.15s",
              }}
              onMouseEnter={e => e.currentTarget.style.borderColor = P.borderLight}
              onMouseLeave={e => e.currentTarget.style.borderColor = P.border}
            >
              ← Terug
            </button>
            <button
              onClick={handleLogin}
              style={{
                flex: 2, padding: "10px",
                background: `linear-gradient(135deg, ${P.blue}, ${P.blueDark})`,
                border: "none", borderRadius: 8,
                color: P.white, cursor: "pointer", fontSize: 14,
                fontWeight: 600, transition: "opacity 0.15s",
              }}
              onMouseEnter={e => e.currentTarget.style.opacity = "0.85"}
              onMouseLeave={e => e.currentTarget.style.opacity = "1"}
            >
              Aanmelden
            </button>
          </div>
        </div>
      )}

      {/* Onderaan: systeem info */}
      <div style={{
        position: "absolute", bottom: 24,
        display: "flex", gap: 24, alignItems: "center",
      }}>
        <Clock />
        <div style={{ width: 1, height: 24, background: P.border }} />
        <div style={{ fontSize: 11, color: P.textDim }}>
          🔒 Versleuteld · 📡 Geen telemetrie · 🇪🇺 Europees
        </div>
      </div>
    </div>
  );
}

// ─── DESKTOP ─────────────────────────────────────────────────
function Desktop({ user, onLogout, onUserSwitch }) {
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [activeApp, setActiveApp] = useState("files");
  const [systemMenuOpen, setSystemMenuOpen] = useState(false);
  const [userMenuOpen, setUserMenuOpen] = useState(false);
  const [notifs, setNotifs] = useState([
    { id: 1, title: "Systeem bijgewerkt", body: "EuroKernel 0.1.2 is geïnstalleerd", icon: "✅", time: "09:14" },
    { id: 2, title: "Beveiligingsmelding", body: "Nieuwe verbinding vanuit lokaal netwerk", icon: "🔒", time: "08:55" },
  ]);
  const [windows, setWindows] = useState([
    { id: "files", title: "Bestanden", icon: "📁", x: 220, y: 60, w: 580, h: 380, active: true },
  ]);
  const [dragging, setDragging] = useState(null);
  const [dragOffset, setDragOffset] = useState({ x: 0, y: 0 });
  const desktopRef = useRef();

  const openApp = (app) => {
    setActiveApp(app.id);
    if (!windows.find(w => w.id === app.id)) {
      setWindows(prev => [
        ...prev.map(w => ({ ...w, active: false })),
        { id: app.id, title: app.label, icon: app.icon,
          x: 200 + prev.length * 30, y: 50 + prev.length * 20,
          w: 560, h: 360, active: true }
      ]);
    } else {
      setWindows(prev => prev.map(w => ({ ...w, active: w.id === app.id })));
    }
    setSystemMenuOpen(false);
  };

  const closeWindow = (id) => {
    setWindows(prev => prev.filter(w => w.id !== id));
  };

  const focusWindow = (id) => {
    setWindows(prev => prev.map(w => ({ ...w, active: w.id === id })));
  };

  const startDrag = (e, win) => {
    if (e.target.closest(".win-btn")) return;
    setDragging(win.id);
    const rect = e.currentTarget.getBoundingClientRect();
    setDragOffset({ x: e.clientX - win.x, y: e.clientY - win.y });
    focusWindow(win.id);
    e.preventDefault();
  };

  useEffect(() => {
    if (!dragging) return;
    const onMove = (e) => {
      setWindows(prev => prev.map(w =>
        w.id === dragging
          ? { ...w, x: e.clientX - dragOffset.x, y: e.clientY - dragOffset.y }
          : w
      ));
    };
    const onUp = () => setDragging(null);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => { window.removeEventListener("mousemove", onMove); window.removeEventListener("mouseup", onUp); };
  }, [dragging, dragOffset]);

  const dismissNotif = (id) => setNotifs(prev => prev.filter(n => n.id !== id));

  const sidebarW = sidebarOpen ? 56 : 20;

  return (
    <div ref={desktopRef} style={{
      width: "100%", height: "100%", position: "relative",
      background: `radial-gradient(ellipse at 20% 80%, #1A3A6B22 0%, transparent 50%),
                   radial-gradient(ellipse at 80% 20%, #0D2A4A33 0%, transparent 50%),
                   ${P.bg}`,
      fontFamily: "'DM Sans', sans-serif",
      overflow: "hidden", userSelect: "none",
    }}
      onClick={() => { setSystemMenuOpen(false); setUserMenuOpen(false); }}
    >
      {/* Achtergrond patroon */}
      <div style={{
        position: "absolute", inset: 0, opacity: 0.025,
        backgroundImage: `linear-gradient(${P.blue} 1px, transparent 1px),
                          linear-gradient(90deg, ${P.blue} 1px, transparent 1px)`,
        backgroundSize: "48px 48px", pointerEvents: "none",
      }} />

      {/* ── ZIJBALK ──────────────────────────────────────── */}
      <div style={{
        position: "absolute", left: 0, top: 0, bottom: 0,
        width: sidebarW, background: P.bgSurface,
        borderRight: `1px solid ${P.border}`,
        display: "flex", flexDirection: "column", alignItems: "center",
        paddingTop: 8, paddingBottom: 8, gap: 4,
        transition: "width 0.2s ease", overflow: "hidden", zIndex: 100,
      }}>
        {/* Logo / Systeem menu knop */}
        <SidebarButton
          onClick={(e) => { e.stopPropagation(); setSystemMenuOpen(v => !v); setUserMenuOpen(false); }}
          active={systemMenuOpen}
          title="Systeem menu"
          highlight
        >
          <span style={{ fontSize: 18, fontWeight: 900, color: P.accent,
                         fontFamily: "'DM Mono', monospace", letterSpacing: "-0.05em" }}>
            €
          </span>
        </SidebarButton>

        <div style={{ width: 32, height: 1, background: P.border, margin: "4px 0" }} />

        {/* App iconen */}
        {APPS.map(app => (
          <SidebarButton
            key={app.id}
            onClick={() => openApp(app)}
            active={windows.some(w => w.id === app.id)}
            title={app.label}
            dot={windows.some(w => w.id === app.id)}
            dotColor={app.color}
          >
            <span style={{ fontSize: 18 }}>{app.icon}</span>
          </SidebarButton>
        ))}

        {/* Spacer */}
        <div style={{ flex: 1 }} />

        {/* Gebruiker knop */}
        <SidebarButton
          onClick={(e) => { e.stopPropagation(); setUserMenuOpen(v => !v); setSystemMenuOpen(false); }}
          active={userMenuOpen}
          title={user.name}
        >
          <div style={{
            width: 28, height: 28, borderRadius: "50%",
            background: user.color + "33", border: `2px solid ${user.color}`,
            display: "flex", alignItems: "center", justifyContent: "center",
            fontSize: 11, fontWeight: 700, color: user.color,
            fontFamily: "'DM Mono', monospace",
          }}>
            {user.avatar}
          </div>
        </SidebarButton>
      </div>

      {/* ── SYSTEEM MENU OVERLAY ─────────────────────────── */}
      {systemMenuOpen && (
        <SystemMenu
          onOpenApp={openApp}
          onClose={() => setSystemMenuOpen(false)}
          user={user}
          onLogout={onLogout}
          style={{ left: sidebarW + 8, top: 8 }}
        />
      )}

      {/* ── GEBRUIKER MENU ───────────────────────────────── */}
      {userMenuOpen && (
        <UserMenu
          currentUser={user}
          users={USERS}
          onSwitch={(u) => { onUserSwitch(u); setUserMenuOpen(false); }}
          onLogout={onLogout}
          style={{ left: sidebarW + 8, bottom: 8 }}
        />
      )}

      {/* ── VENSTERS ─────────────────────────────────────── */}
      {windows.map(win => (
        <Window
          key={win.id}
          win={win}
          onClose={() => closeWindow(win.id)}
          onFocus={() => focusWindow(win.id)}
          onStartDrag={(e) => startDrag(e, win)}
          user={user}
        />
      ))}

      {/* ── STATUSBALK RECHTSBOVENHOEK ────────────────────── */}
      <div style={{
        position: "absolute", top: 12, right: 16,
        display: "flex", alignItems: "center", gap: 14,
        zIndex: 200,
      }}>
        <div style={{ fontSize: 12, color: P.textSec }}>🔒</div>
        <div style={{ fontSize: 12, color: P.textSec }}>📡 Lokaal</div>
        <div style={{ fontSize: 12, color: P.green }}>● Online</div>
        <div style={{ width: 1, height: 16, background: P.border }} />
        <Clock />
      </div>

      {/* ── NOTIFICATIES ─────────────────────────────────── */}
      <div style={{
        position: "absolute", bottom: 16, right: 16,
        display: "flex", flexDirection: "column", gap: 8, zIndex: 300,
      }}>
        {notifs.map(n => (
          <Notification key={n.id} notif={n} onDismiss={() => dismissNotif(n.id)} />
        ))}
      </div>
    </div>
  );
}

// ─── SIDEBAR KNOP ────────────────────────────────────────────
function SidebarButton({ children, onClick, active, title, dot, dotColor, highlight }) {
  const [hov, setHov] = useState(false);
  return (
    <div style={{ position: "relative" }}>
      <button
        onClick={onClick}
        title={title}
        onMouseEnter={() => setHov(true)}
        onMouseLeave={() => setHov(false)}
        style={{
          width: 40, height: 40, borderRadius: 10,
          background: active ? (highlight ? P.accent + "22" : P.bgHover) : hov ? P.bgCard : "transparent",
          border: `1px solid ${active ? (highlight ? P.accent + "55" : P.borderLight) : "transparent"}`,
          display: "flex", alignItems: "center", justifyContent: "center",
          cursor: "pointer", color: active ? P.textPrim : P.textSec,
          transition: "all 0.15s", padding: 0,
        }}
      >
        {children}
      </button>
      {dot && (
        <div style={{
          position: "absolute", bottom: 3, right: 3,
          width: 6, height: 6, borderRadius: "50%",
          background: dotColor || P.accent,
          border: `1.5px solid ${P.bgSurface}`,
        }} />
      )}
    </div>
  );
}

// ─── SYSTEEM MENU ─────────────────────────────────────────────
function SystemMenu({ onOpenApp, onClose, user, onLogout, style }) {
  const [search, setSearch] = useState("");
  const filtered = APPS.filter(a =>
    a.label.toLowerCase().includes(search.toLowerCase())
  );

  return (
    <div
      onClick={e => e.stopPropagation()}
      style={{
        position: "absolute", zIndex: 500,
        width: 320,
        background: P.bgCard,
        border: `1px solid ${P.border}`,
        borderRadius: 14,
        boxShadow: `0 24px 64px #00000066, 0 0 0 1px ${P.border}`,
        overflow: "hidden",
        animation: "popIn 0.15s ease",
        ...style,
      }}
    >
      <style>{`
        @keyframes popIn {
          from { opacity: 0; transform: scale(0.95) translateY(-4px); }
          to   { opacity: 1; transform: scale(1) translateY(0); }
        }
      `}</style>

      {/* Header */}
      <div style={{
        padding: "16px 16px 12px",
        borderBottom: `1px solid ${P.border}`,
        background: `linear-gradient(135deg, ${P.bgCard}, ${P.bgSurface})`,
      }}>
        <div style={{ fontSize: 11, color: P.accent, fontFamily: "'DM Mono', monospace",
                      letterSpacing: "0.2em", marginBottom: 4 }}>
          EUROKERNELOS v0.1
        </div>
        <div style={{ fontSize: 13, color: P.textSec }}>
          Aangemeld als <span style={{ color: user.color, fontWeight: 600 }}>{user.name}</span>
          {" "}<RoleBadge role={user.role} />
        </div>
      </div>

      {/* Zoekbalk */}
      <div style={{ padding: "10px 12px", borderBottom: `1px solid ${P.border}` }}>
        <input
          autoFocus
          value={search}
          onChange={e => setSearch(e.target.value)}
          placeholder="Zoek applicaties..."
          style={{
            width: "100%", padding: "8px 12px",
            background: P.bgSurface, border: `1px solid ${P.border}`,
            borderRadius: 8, color: P.textPrim, fontSize: 13,
            outline: "none", boxSizing: "border-box",
          }}
        />
      </div>

      {/* Applicaties */}
      <div style={{ padding: "8px 8px" }}>
        <div style={{ fontSize: 10, color: P.textDim, padding: "4px 8px",
                      letterSpacing: "0.1em", fontFamily: "'DM Mono', monospace" }}>
          APPLICATIES
        </div>
        {filtered.map(app => (
          <MenuRow key={app.id} onClick={() => { onOpenApp(app); onClose(); }}>
            <span style={{ fontSize: 18, marginRight: 10 }}>{app.icon}</span>
            <span style={{ flex: 1, fontSize: 14 }}>{app.label}</span>
          </MenuRow>
        ))}
      </div>

      {/* Systeem acties */}
      <div style={{ borderTop: `1px solid ${P.border}`, padding: "8px 8px" }}>
        <div style={{ fontSize: 10, color: P.textDim, padding: "4px 8px",
                      letterSpacing: "0.1em", fontFamily: "'DM Mono', monospace" }}>
          SYSTEEM
        </div>
        <MenuRow onClick={onClose}>
          <span style={{ fontSize: 16, marginRight: 10 }}>💤</span>
          <span style={{ flex: 1, fontSize: 14 }}>Slaapstand</span>
        </MenuRow>
        <MenuRow onClick={onClose}>
          <span style={{ fontSize: 16, marginRight: 10 }}>🔄</span>
          <span style={{ flex: 1, fontSize: 14 }}>Herstarten</span>
        </MenuRow>
        <MenuRow onClick={onLogout} danger>
          <span style={{ fontSize: 16, marginRight: 10 }}>⏻</span>
          <span style={{ flex: 1, fontSize: 14 }}>Afmelden / Afsluiten</span>
        </MenuRow>
      </div>
    </div>
  );
}

// ─── GEBRUIKER MENU ───────────────────────────────────────────
function UserMenu({ currentUser, users, onSwitch, onLogout, style }) {
  return (
    <div
      onClick={e => e.stopPropagation()}
      style={{
        position: "absolute", zIndex: 500,
        width: 280,
        background: P.bgCard,
        border: `1px solid ${P.border}`,
        borderRadius: 14,
        boxShadow: `0 24px 64px #00000066`,
        overflow: "hidden",
        animation: "popIn 0.15s ease",
        ...style,
      }}
    >
      {/* Huidige gebruiker header */}
      <div style={{
        padding: "16px",
        borderBottom: `1px solid ${P.border}`,
        display: "flex", alignItems: "center", gap: 12,
      }}>
        <Avatar user={currentUser} size={44} ring />
        <div>
          <div style={{ fontWeight: 700, color: P.textPrim, fontSize: 15 }}>
            {currentUser.name}
          </div>
          <RoleBadge role={currentUser.role} />
        </div>
      </div>

      {/* Andere gebruikers */}
      <div style={{ padding: "8px 8px" }}>
        <div style={{ fontSize: 10, color: P.textDim, padding: "4px 8px",
                      letterSpacing: "0.1em", fontFamily: "'DM Mono', monospace" }}>
          WISSEL VAN GEBRUIKER
        </div>
        {users.filter(u => u.id !== currentUser.id).map(u => (
          <button key={u.id}
            onClick={() => onSwitch(u)}
            style={{
              width: "100%", display: "flex", alignItems: "center", gap: 10,
              padding: "8px 10px", background: "transparent",
              border: "none", borderRadius: 8, cursor: "pointer",
              transition: "background 0.1s", color: P.textPrim,
            }}
            onMouseEnter={e => e.currentTarget.style.background = P.bgHover}
            onMouseLeave={e => e.currentTarget.style.background = "transparent"}
          >
            <Avatar user={u} size={32} />
            <div style={{ textAlign: "left", flex: 1 }}>
              <div style={{ fontSize: 14, fontWeight: 500 }}>{u.name}</div>
              <div style={{ fontSize: 10, color: P.textSec }}>{u.lastLogin}</div>
            </div>
            <RoleBadge role={u.role} />
          </button>
        ))}
      </div>

      {/* Gebruikersbeheer (alleen voor admin) */}
      {currentUser.role === "admin" && (
        <div style={{ borderTop: `1px solid ${P.border}`, padding: "8px 8px" }}>
          <MenuRow onClick={() => {}}>
            <span style={{ fontSize: 14, marginRight: 10 }}>👤</span>
            <span style={{ fontSize: 13, flex: 1 }}>Gebruikersbeheer</span>
            <span style={{ fontSize: 10, color: P.textDim, fontFamily: "'DM Mono', monospace",
                           background: P.bgHover, padding: "2px 6px", borderRadius: 4 }}>
              ADMIN
            </span>
          </MenuRow>
          <MenuRow onClick={() => {}}>
            <span style={{ fontSize: 14, marginRight: 10 }}>🔐</span>
            <span style={{ fontSize: 13, flex: 1 }}>Permissies beheren</span>
          </MenuRow>
        </div>
      )}

      <div style={{ borderTop: `1px solid ${P.border}`, padding: "8px 8px" }}>
        <MenuRow onClick={onLogout} danger>
          <span style={{ fontSize: 14, marginRight: 10 }}>🚪</span>
          <span style={{ fontSize: 13, flex: 1 }}>Afmelden</span>
        </MenuRow>
      </div>
    </div>
  );
}

// ─── MENU RIJ ─────────────────────────────────────────────────
function MenuRow({ children, onClick, danger }) {
  const [hov, setHov] = useState(false);
  return (
    <button
      onClick={onClick}
      onMouseEnter={() => setHov(true)}
      onMouseLeave={() => setHov(false)}
      style={{
        width: "100%", display: "flex", alignItems: "center",
        padding: "8px 10px", borderRadius: 8, border: "none",
        background: hov ? (danger ? P.red + "18" : P.bgHover) : "transparent",
        color: hov && danger ? P.red : P.textPrim,
        cursor: "pointer", transition: "all 0.1s", textAlign: "left",
      }}
    >
      {children}
    </button>
  );
}

// ─── VENSTER ─────────────────────────────────────────────────
function Window({ win, onClose, onFocus, onStartDrag, user }) {
  return (
    <div
      onMouseDown={onFocus}
      style={{
        position: "absolute",
        left: win.x, top: win.y,
        width: win.w, height: win.h,
        background: P.bgCard,
        border: `1px solid ${win.active ? P.borderLight : P.border}`,
        borderRadius: 12,
        boxShadow: win.active
          ? `0 32px 80px #00000055, 0 0 0 1px ${P.borderLight}`
          : `0 8px 24px #00000033`,
        overflow: "hidden",
        transition: "box-shadow 0.2s",
        zIndex: win.active ? 50 : 10,
      }}
    >
      {/* Titelbalk */}
      <div
        onMouseDown={onStartDrag}
        style={{
          height: 38, display: "flex", alignItems: "center",
          padding: "0 12px", gap: 10,
          background: win.active ? P.bgSurface : P.bg,
          borderBottom: `1px solid ${P.border}`,
          cursor: "grab",
        }}
      >
        {/* Venster knoppen */}
        <div className="win-btn" style={{ display: "flex", gap: 6 }}>
          <WinBtn color={P.red}    onClick={onClose} title="Sluiten" />
          <WinBtn color={P.yellow} onClick={() => {}} title="Minimaliseren" />
          <WinBtn color={P.green}  onClick={() => {}} title="Maximaliseren" />
        </div>

        {/* Titel */}
        <div style={{ flex: 1, textAlign: "center", fontSize: 12,
                      fontWeight: 600, color: win.active ? P.textSec : P.textDim }}>
          {win.icon} {win.title}
        </div>

        {/* Gebruiker indicator */}
        <div style={{
          fontSize: 10, color: user.color, fontFamily: "'DM Mono', monospace",
          background: user.color + "18", padding: "2px 7px", borderRadius: 4,
          border: `1px solid ${user.color}33`,
        }}>
          {user.name}
        </div>
      </div>

      {/* Venster inhoud */}
      <WindowContent id={win.id} user={user} />
    </div>
  );
}

function WinBtn({ color, onClick, title }) {
  const [hov, setHov] = useState(false);
  return (
    <button
      className="win-btn"
      onClick={onClick}
      title={title}
      onMouseEnter={() => setHov(true)}
      onMouseLeave={() => setHov(false)}
      style={{
        width: 13, height: 13, borderRadius: "50%",
        background: hov ? color : color + "88",
        border: `1px solid ${color}55`,
        cursor: "pointer", padding: 0, transition: "background 0.1s",
      }}
    />
  );
}

// ─── VENSTER INHOUD ───────────────────────────────────────────
function WindowContent({ id, user }) {
  if (id === "files") return <FilesApp user={user} />;
  if (id === "settings") return <SettingsApp user={user} />;
  if (id === "terminal") return <TerminalApp user={user} />;
  return (
    <div style={{
      display: "flex", alignItems: "center", justifyContent: "center",
      height: "calc(100% - 38px)", color: P.textSec, fontSize: 14,
    }}>
      <div style={{ textAlign: "center" }}>
        <div style={{ fontSize: 32, marginBottom: 8 }}>
          {APPS.find(a => a.id === id)?.icon}
        </div>
        <div>{APPS.find(a => a.id === id)?.label}</div>
        <div style={{ fontSize: 11, color: P.textDim, marginTop: 4 }}>
          Applicatie wordt geladen...
        </div>
      </div>
    </div>
  );
}

function FilesApp({ user }) {
  const items = [
    { name: "Documenten",  icon: "📁", type: "map",     size: "—",      date: "vandaag" },
    { name: "Afbeeldingen",icon: "🖼️", type: "map",     size: "—",      date: "gisteren" },
    { name: "Downloads",   icon: "📥", type: "map",     size: "—",      date: "3 jun" },
    { name: "rapport.pdf", icon: "📄", type: "bestand", size: "2.4 MB", date: "vandaag" },
    { name: "notities.txt",icon: "📝", type: "bestand", size: "12 KB",  date: "gisteren" },
    { name: "config.yaml", icon: "⚙️", type: "bestand", size: "3 KB",   date: "1 jun" },
  ];
  return (
    <div style={{ height: "calc(100% - 38px)", display: "flex", flexDirection: "column" }}>
      {/* Toolbar */}
      <div style={{
        display: "flex", alignItems: "center", gap: 8, padding: "8px 12px",
        borderBottom: `1px solid ${P.border}`, background: P.bgSurface,
      }}>
        <span style={{ fontSize: 11, color: P.textSec }}>🏠 Thuis</span>
        <span style={{ color: P.textDim }}>›</span>
        <span style={{ fontSize: 11, color: P.textPrim }}>{user.name}</span>
        <div style={{ flex: 1 }} />
        <div style={{ fontSize: 11, color: P.textSec }}>EuroFS v0.1 · Versleuteld</div>
      </div>
      {/* Lijst */}
      <div style={{ flex: 1, overflow: "auto", padding: "4px 0" }}>
        {items.map((item, i) => (
          <div key={i} style={{
            display: "flex", alignItems: "center", gap: 10,
            padding: "7px 16px", cursor: "pointer", transition: "background 0.1s",
          }}
            onMouseEnter={e => e.currentTarget.style.background = P.bgHover}
            onMouseLeave={e => e.currentTarget.style.background = "transparent"}
          >
            <span style={{ fontSize: 16 }}>{item.icon}</span>
            <span style={{ flex: 1, fontSize: 13, color: P.textPrim }}>{item.name}</span>
            <span style={{ fontSize: 11, color: P.textDim, width: 60, textAlign: "right" }}>{item.size}</span>
            <span style={{ fontSize: 11, color: P.textDim, width: 70, textAlign: "right" }}>{item.date}</span>
          </div>
        ))}
      </div>
      <div style={{
        padding: "6px 12px", borderTop: `1px solid ${P.border}`,
        fontSize: 11, color: P.textDim, display: "flex", justifyContent: "space-between",
      }}>
        <span>6 items</span>
        <span style={{ color: P.green + "cc" }}>🔒 EuroFS · Versleuteld</span>
      </div>
    </div>
  );
}

function SettingsApp({ user }) {
  const sections = [
    { icon: "👤", label: "Gebruikers & Toegang" },
    { icon: "🎨", label: "Weergave" },
    { icon: "🔒", label: "Beveiliging & Privacy" },
    { icon: "📡", label: "Netwerk" },
    { icon: "🔔", label: "Meldingen" },
    { icon: "♿", label: "Toegankelijkheid" },
    { icon: "💾", label: "Opslag (EuroFS)" },
    { icon: "⚡", label: "Energie" },
  ];
  return (
    <div style={{ height: "calc(100% - 38px)", display: "flex" }}>
      {/* Zijbalk instellingen */}
      <div style={{
        width: 180, borderRight: `1px solid ${P.border}`,
        padding: "8px 0", background: P.bgSurface,
      }}>
        {sections.map((s, i) => (
          <div key={i} style={{
            display: "flex", alignItems: "center", gap: 8,
            padding: "8px 14px", cursor: "pointer",
            fontSize: 13, color: i === 0 ? P.accent : P.textSec,
            background: i === 0 ? P.accent + "11" : "transparent",
            borderLeft: i === 0 ? `2px solid ${P.accent}` : "2px solid transparent",
            transition: "all 0.1s",
          }}
            onMouseEnter={e => { if (i !== 0) { e.currentTarget.style.background = P.bgHover; e.currentTarget.style.color = P.textPrim; }}}
            onMouseLeave={e => { if (i !== 0) { e.currentTarget.style.background = "transparent"; e.currentTarget.style.color = P.textSec; }}}
          >
            <span>{s.icon}</span>
            <span>{s.label}</span>
          </div>
        ))}
      </div>
      {/* Inhoud */}
      <div style={{ flex: 1, padding: 20, overflowY: "auto" }}>
        <div style={{ fontSize: 15, fontWeight: 700, color: P.textPrim, marginBottom: 16 }}>
          👤 Gebruikers & Toegang
        </div>
        {USERS.map(u => (
          <div key={u.id} style={{
            display: "flex", alignItems: "center", gap: 12,
            padding: "10px 14px", background: P.bgSurface,
            borderRadius: 8, marginBottom: 8,
            border: `1px solid ${u.id === user.id ? P.borderLight : P.border}`,
          }}>
            <Avatar user={u} size={36} ring={u.id === user.id} />
            <div style={{ flex: 1 }}>
              <div style={{ fontSize: 14, fontWeight: 600, color: P.textPrim }}>
                {u.name} {u.id === user.id && <span style={{ fontSize: 10, color: P.accent }}>(jij)</span>}
              </div>
              <div style={{ fontSize: 11, color: P.textSec }}>Laatste login: {u.lastLogin}</div>
            </div>
            <RoleBadge role={u.role} />
            {user.role === "admin" && u.id !== user.id && (
              <button style={{
                fontSize: 11, padding: "4px 10px", borderRadius: 6,
                background: P.bgHover, border: `1px solid ${P.border}`,
                color: P.textSec, cursor: "pointer",
              }}>
                Bewerken
              </button>
            )}
          </div>
        ))}
        {user.role === "admin" && (
          <button style={{
            display: "flex", alignItems: "center", gap: 8,
            padding: "10px 14px", background: "transparent",
            border: `1px dashed ${P.borderLight}`, borderRadius: 8,
            color: P.accent, cursor: "pointer", fontSize: 13,
            width: "100%",
          }}>
            <span>＋</span> Nieuwe gebruiker toevoegen
          </button>
        )}
      </div>
    </div>
  );
}

function TerminalApp({ user }) {
  const [lines, setLines] = useState([
    `EuroKernel OS v0.1 — Terminal`,
    `Ingelogd als: ${user.name} (${user.role})`,
    `Type 'help' voor beschikbare commando's`,
    ``,
  ]);
  const [input, setInput] = useState("");
  const bottomRef = useRef();

  useEffect(() => bottomRef.current?.scrollIntoView(), [lines]);

  const handleCommand = (e) => {
    if (e.key !== "Enter") return;
    const cmd = input.trim();
    const newLines = [...lines, `${user.name}@eurokernel:~$ ${cmd}`];
    if (cmd === "help") newLines.push("Beschikbare commando's: help, whoami, uname, ls, clear");
    else if (cmd === "whoami") newLines.push(`${user.name} (${user.role})`);
    else if (cmd === "uname") newLines.push("EuroKernel OS v0.1-alpha x86_64");
    else if (cmd === "ls") newLines.push("Documenten/  Afbeeldingen/  Downloads/  rapport.pdf  notities.txt");
    else if (cmd === "clear") { setLines([]); setInput(""); return; }
    else if (cmd) newLines.push(`commando niet gevonden: ${cmd}`);
    newLines.push("");
    setLines(newLines);
    setInput("");
  };

  return (
    <div style={{
      height: "calc(100% - 38px)", background: "#0A0F1A",
      padding: "12px 16px", fontFamily: "'DM Mono', monospace",
      fontSize: 13, color: "#A8D8A8", overflow: "auto",
      display: "flex", flexDirection: "column",
    }}>
      <div style={{ flex: 1 }}>
        {lines.map((l, i) => (
          <div key={i} style={{ lineHeight: 1.6, whiteSpace: "pre-wrap" }}>
            {l || "\u00A0"}
          </div>
        ))}
        <div ref={bottomRef} style={{ display: "flex", alignItems: "center", gap: 6 }}>
          <span style={{ color: P.accent }}>{user.name}@eurokernel:~$</span>
          <input
            autoFocus
            value={input}
            onChange={e => setInput(e.target.value)}
            onKeyDown={handleCommand}
            style={{
              flex: 1, background: "transparent", border: "none",
              color: "#A8D8A8", fontSize: 13, outline: "none",
              fontFamily: "'DM Mono', monospace",
            }}
          />
        </div>
      </div>
    </div>
  );
}

// ─── NOTIFICATIE ──────────────────────────────────────────────
function Notification({ notif, onDismiss }) {
  return (
    <div style={{
      width: 300, background: P.bgCard,
      border: `1px solid ${P.border}`,
      borderRadius: 10, padding: "12px 14px",
      boxShadow: "0 8px 32px #00000044",
      display: "flex", gap: 10, alignItems: "flex-start",
      animation: "slideIn 0.2s ease",
    }}>
      <style>{`
        @keyframes slideIn {
          from { opacity: 0; transform: translateX(20px); }
          to   { opacity: 1; transform: translateX(0); }
        }
      `}</style>
      <span style={{ fontSize: 18 }}>{notif.icon}</span>
      <div style={{ flex: 1 }}>
        <div style={{ fontSize: 13, fontWeight: 600, color: P.textPrim }}>{notif.title}</div>
        <div style={{ fontSize: 11, color: P.textSec, marginTop: 2 }}>{notif.body}</div>
      </div>
      <button onClick={onDismiss} style={{
        background: "none", border: "none", color: P.textDim,
        cursor: "pointer", fontSize: 14, padding: 0, lineHeight: 1,
      }}>✕</button>
    </div>
  );
}

// ─── ROOT APP ─────────────────────────────────────────────────
export default function EuroOS() {
  const [loggedInUser, setLoggedInUser] = useState(null);

  return (
    <div style={{
      width: "100%", height: "100vh", overflow: "hidden",
      fontFamily: "'DM Sans', sans-serif",
    }}>
      <style>{`
        @import url('https://fonts.googleapis.com/css2?family=DM+Sans:wght@400;500;600;700;800&family=DM+Mono:wght@400;500;700&display=swap');
        * { box-sizing: border-box; }
        ::-webkit-scrollbar { width: 6px; }
        ::-webkit-scrollbar-track { background: transparent; }
        ::-webkit-scrollbar-thumb { background: ${P.border}; border-radius: 3px; }
        button { font-family: inherit; }
        input { font-family: inherit; }
      `}</style>

      {!loggedInUser ? (
        <LoginScreen onLogin={setLoggedInUser} />
      ) : (
        <Desktop
          user={loggedInUser}
          onLogout={() => setLoggedInUser(null)}
          onUserSwitch={(u) => setLoggedInUser(u)}
        />
      )}
    </div>
  );
}
