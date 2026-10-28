// FramePlayerX web remote. Every string from the server is rendered with
// textContent or as an attribute value; nothing is ever parsed as HTML.
'use strict';

(function () {
  const $ = (id) => document.getElementById(id);
  const el = {
    conn: $('conn'), connText: $('conn-text'),
    idle: $('idle'), player: $('player'),
    picture: $('picture'), format: $('format'), title: $('title'), marker: $('marker'), close: $('close'),
    segments: $('segments'), seekKnob: $('seek-knob'), seek: $('seek'), pos: $('pos'), dur: $('dur'),
    prev: $('prev'), back: $('back'), play: $('play'), iconPlay: $('icon-play'), iconPause: $('icon-pause'),
    fwd: $('fwd'), next: $('next'),
    mute: $('mute'), iconSound: $('icon-sound'), iconMuted: $('icon-muted'),
    volume: $('volume'), volumeFill: $('volume-fill'), volumeKnob: $('volume-knob'), volumeText: $('volume-text'),
    audio: $('audio'), audioValue: $('audio-value'),
    subtitles: $('subtitles'), subtitlesValue: $('subtitles-value'),
    speed: $('speed'), speedValue: $('speed-value'), recenter: $('recenter'),
    upNext: $('up-next'), upNextValue: $('up-next-value'),
    loopVideo: $('loop-video'), loopVideoValue: $('loop-video-value'),
    autoplay: $('autoplay'), autoplayValue: $('autoplay-value'),
    backdrop: $('backdrop'), sheet: $('sheet'), sheetTitle: $('sheet-title'), sheetBody: $('sheet-body'),
    sheetClose: $('sheet-close'),
    typeForm: $('type-form'), typeText: $('type-text'), typeStatus: $('type-status'),
  };

  // The page's text in the headset's language, which filled the page's
  // own text as it was served; t('id', 'English', {name: value}).
  let strings = {};
  try { strings = JSON.parse($('strings').textContent) || {}; } catch (_) { strings = {}; }
  const lang = document.documentElement.lang;
  function t(id, english, args) {
    let s = typeof strings[id] === 'string' ? strings[id] : english;
    for (const k of Object.keys(args || {})) s = s.split('{' + k + '}').join(String(args[k]));
    return s;
  }

  const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];
  let status = null;        // last status from the server
  let details = {};         // the open video's details
  let pictureShown = null;  // the number of the picture showing
  let segmentsFor = '';     // the duration and markers the seek bar shows
  let clockOffset = 0;      // phone clock minus server clock, ms
  let seeking = false;      // a finger on the seek bar
  let changingVolume = false;
  let volumeSentAt = 0;
  let unpaired = false;
  let sheet = null;         // 'tracks', 'speed' or 'up-next' while one is open
  let typeTimer = 0;

  function setConn(text, state) {
    el.connText.textContent = text;
    el.conn.classList.toggle('ok', state === 'ok');
    el.conn.classList.toggle('bad', state === 'bad');
  }

  function fmt(t) {
    if (!isFinite(t) || t < 0) t = 0;
    t = Math.floor(t);
    const h = Math.floor(t / 3600);
    const m = Math.floor((t % 3600) / 60);
    const s = String(t % 60).padStart(2, '0');
    return h > 0 ? h + ':' + String(m).padStart(2, '0') + ':' + s : m + ':' + s;
  }

  function api(path, opts) {
    const o = Object.assign({ credentials: 'same-origin', cache: 'no-store' }, opts || {});
    return fetch(path, o).then((r) => {
      if (r.status === 401) {
        // Unpaired in the headset: reloaded, this becomes the pairing page.
        unpaired = true;
        location.reload();
        throw new Error('not paired');
      }
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r;
    });
  }

  function postJson(path, body) {
    return api(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
  }

  function send(cmd) {
    return postJson('/api/command', cmd)
      .catch((e) => setConn(t('remote-command-failed', 'Command failed: {error}', { error: e.message }), 'bad'));
  }

  function isOpen() {
    return !!status && typeof status.location === 'string' && status.location.length > 0;
  }

  function position() {
    if (!status) return 0;
    if (!status.playing) return status.position;
    const serverNow = Date.now() - clockOffset;
    const p = status.position + Math.max(0, serverNow - status.sampled_at_ms) / 1000 * status.speed;
    return status.duration > 0 ? Math.min(p, status.duration) : p;
  }

  function speedText(v) {
    return (Math.round(v * 100) / 100) + '×';
  }

  function applyStatus(s) {
    if (!s || typeof s !== 'object') return;
    // The headset changed its language: the page follows.
    if (s.lang && lang && s.lang !== lang) { location.reload(); return; }
    clockOffset = Date.now() - Number(s.now_ms || Date.now());
    status = s;
    details = s.details && typeof s.details === 'object' ? s.details : {};
    const open = isOpen();
    el.idle.hidden = open;
    el.player.hidden = !open;
    if (!open) { closeSheet(); return; }

    el.title.textContent = s.title || s.location;
    el.play.setAttribute('aria-label', s.playing ? t('remote-pause', 'Pause') : t('remote-play', 'Play'));
    // An SVG element has no `hidden` property: the attribute it is.
    el.iconPlay.toggleAttribute('hidden', !!s.playing);
    el.iconPause.toggleAttribute('hidden', !s.playing);
    el.seek.disabled = !(s.duration > 0);
    el.seek.max = String(s.duration > 0 ? s.duration : 0);
    el.dur.textContent = fmt(s.duration);
    el.speedValue.textContent = speedText(Number(s.speed) || 1);
    applyDetails(s.picture);
    tick();
    if (sheet) renderSheet();
  }

  function applyDetails(picture) {
    const d = details;
    if (typeof picture !== 'number') {
      pictureShown = null;
      el.picture.hidden = true;
      el.picture.removeAttribute('src');
    } else if (picture !== pictureShown) {
      pictureShown = picture;
      el.picture.src = '/api/picture?v=' + encodeURIComponent(String(picture));
      el.picture.hidden = false;
    }
    el.format.textContent = d.format || '';
    el.format.hidden = !d.format;
    setSegments(Array.isArray(d.markers) ? d.markers : []);
    el.prev.disabled = !d.has_previous;
    el.next.disabled = !d.has_next;
    if (!changingVolume) showVolume(Number(d.volume));
    const audio = (Array.isArray(d.audio) ? d.audio : []).find((a) => a.selected);
    el.audioValue.textContent = audio ? String(audio.label) : t('remote-no-audio', 'No audio');
    const subs = (Array.isArray(d.subtitles) ? d.subtitles : []).find((x) => x.selected);
    el.subtitlesValue.textContent = subs ? String(subs.label) : t('remote-subtitles-off', 'Off');
    const queue = Array.isArray(d.up_next) ? d.up_next : [];
    el.upNextValue.textContent = queue.length > 0
      ? t('remote-queue-position', '{at} of {of}', { at: Number(d.up_next_at) + 1, of: queue.length })
      : t('remote-nothing-queued', 'Nothing queued');
    showSwitches();
  }

  function onOff(on) {
    return on ? t('remote-on', 'On') : t('remote-off', 'Off');
  }

  function showSwitches() {
    for (const [button, value, on] of [
      [el.loopVideo, el.loopVideoValue, !!details.loop_video],
      [el.autoplay, el.autoplayValue, !!details.autoplay],
    ]) {
      button.setAttribute('aria-pressed', on ? 'true' : 'false');
      value.textContent = onOff(on);
    }
  }

  // Turns a switch over at once; the next status confirms it.
  function flip(key, cmd) {
    const on = !details[key];
    details[key] = on;
    showSwitches();
    if (sheet) renderSheet();
    send({ cmd: cmd, on: on });
  }

  // The seek bar is split where chapters and bookmarks start.
  function setSegments(markers) {
    const dur = status && status.duration > 0 ? status.duration : 0;
    const starts = [0];
    for (const m of markers) {
      const at = Number(m.time);
      if (dur > 0 && at > 0.5 && at < dur - 0.5) starts.push(at);
    }
    starts.sort((a, b) => a - b);
    const key = dur + '|' + starts.join(',');
    if (key === segmentsFor) return;
    segmentsFor = key;
    el.segments.replaceChildren();
    starts.forEach((start, i) => {
      const end = i + 1 < starts.length ? starts[i + 1] : dur;
      const part = document.createElement('span');
      part.dataset.start = String(start);
      part.dataset.end = String(end);
      part.style.flexGrow = String(dur > 0 ? Math.max(end - start, 0.001) : 1);
      el.segments.appendChild(part);
    });
  }

  function paintSeek(p) {
    const dur = status && status.duration > 0 ? status.duration : 0;
    const frac = dur > 0 ? Math.min(Math.max(p / dur, 0), 1) : 0;
    el.seekKnob.style.left = (frac * 100) + '%';
    for (const part of el.segments.children) {
      const a = Number(part.dataset.start);
      const b = Number(part.dataset.end);
      const f = b > a ? Math.min(Math.max((p - a) / (b - a), 0), 1) : 0;
      const at = (f * 100).toFixed(2) + '%';
      part.style.background = f >= 1 ? 'var(--accent)'
        : f <= 0 ? '' : 'linear-gradient(90deg, var(--accent) ' + at + ', var(--surface-3) ' + at + ')';
    }
    let name = '';
    for (const m of Array.isArray(details.markers) ? details.markers : []) {
      if (Number(m.time) <= p + 0.25) name = String(m.name || '');
    }
    el.marker.textContent = name;
    el.marker.hidden = !name;
  }

  function tick() {
    if (!isOpen()) return;
    const p = seeking ? Number(el.seek.value) : position();
    if (!seeking) el.seek.value = String(p);
    el.pos.textContent = fmt(p);
    paintSeek(p);
    el.seek.setAttribute('aria-valuetext',
      t('remote-position', '{position} of {duration}', { position: fmt(p), duration: fmt(status.duration) }));
  }

  function showVolume(v) {
    if (!isFinite(v)) v = 1;
    const percent = Math.round(Math.min(Math.max(v, 0), 1.5) * 100);
    const along = (percent / 150 * 100) + '%';
    el.volume.value = String(percent);
    el.volumeFill.style.width = along;
    el.volumeKnob.style.left = along;
    el.volumeText.textContent = percent + '%';
    const muted = percent === 0;
    el.iconSound.toggleAttribute('hidden', muted);
    el.iconMuted.toggleAttribute('hidden', !muted);
    el.mute.setAttribute('aria-label', muted ? t('remote-unmute', 'Unmute') : t('remote-mute', 'Mute'));
  }

  // Sheets: the audio and subtitle choices, the speeds, and Up next.
  function openSheet(kind) {
    sheet = kind;
    renderSheet();
    el.backdrop.hidden = false;
    el.sheet.hidden = false;
    el.sheetClose.focus();
  }

  function closeSheet() {
    if (!sheet) return;
    sheet = null;
    el.backdrop.hidden = true;
    el.sheet.hidden = true;
  }

  function check() {
    return $('check').content.firstElementChild.cloneNode(true);
  }

  function option(label, detail, selected, pick) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'option';
    b.setAttribute('aria-pressed', selected ? 'true' : 'false');
    const text = document.createElement('span');
    text.textContent = label;
    if (detail) {
      const d = document.createElement('span');
      d.className = 'detail';
      d.textContent = ' · ' + detail;
      text.appendChild(d);
    }
    b.appendChild(text);
    if (selected) b.appendChild(check());
    b.addEventListener('click', () => {
      if (!selected) pick();
      closeSheet();
    });
    return b;
  }

  // A switch in a sheet: says On or Off, and the sheet stays open.
  function switchRow(label, on, flipIt) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'option';
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
    const text = document.createElement('span');
    text.textContent = label;
    const state = document.createElement('span');
    state.className = 'detail';
    state.textContent = onOff(on);
    b.append(text, state);
    b.addEventListener('click', flipIt);
    return b;
  }

  function heading(text) {
    const h = document.createElement('h3');
    h.textContent = text;
    return h;
  }

  function renderSheet() {
    const body = [];
    if (sheet === 'tracks') {
      el.sheetTitle.textContent = t('remote-tracks', 'Audio and subtitles');
      const audio = Array.isArray(details.audio) ? details.audio : [];
      body.push(heading(t('remote-audio', 'Audio')));
      if (audio.length === 0) {
        const p = document.createElement('p');
        p.textContent = t('remote-no-audio', 'No audio');
        body.push(p);
      }
      for (const a of audio) {
        body.push(option(String(a.label), String(a.detail || ''), !!a.selected,
          () => send({ cmd: 'select_audio', stream: a.stream })));
      }
      const subs = Array.isArray(details.subtitles) ? details.subtitles : [];
      body.push(heading(t('remote-subtitles', 'Subtitles')));
      body.push(option(t('remote-subtitles-off', 'Off'), '', !subs.some((x) => x.selected),
        () => send({ cmd: 'select_subtitles', track: null })));
      for (const x of subs) {
        body.push(option(String(x.label), String(x.detail || ''), !!x.selected,
          () => send({ cmd: 'select_subtitles', track: x.track })));
      }
    } else if (sheet === 'speed') {
      el.sheetTitle.textContent = t('remote-speed', 'Speed');
      const now = status ? Number(status.speed) || 1 : 1;
      for (const v of SPEEDS) {
        body.push(option(speedText(v), '', Math.abs(v - now) < 0.001,
          () => send({ cmd: 'set_speed', speed: v })));
      }
    } else if (sheet === 'up-next') {
      el.sheetTitle.textContent = t('remote-up-next', 'Up next');
      body.push(switchRow(t('remote-loop', 'Loop'), !!details.loop_playlist,
        () => flip('loop_playlist', 'set_loop_playlist')));
      const queue = Array.isArray(details.up_next) ? details.up_next : [];
      if (queue.length === 0) {
        const p = document.createElement('p');
        p.textContent = t('remote-nothing-queued', 'Nothing queued');
        body.push(p);
      }
      queue.forEach((title, i) => {
        const playing = i === Number(details.up_next_at);
        body.push(option(String(title), playing ? t('remote-now-playing', 'Now playing') : '', playing,
          () => send({ cmd: 'play_queued', index: i })));
      });
    }
    el.sheetBody.replaceChildren(...body);
  }

  // The player
  el.play.addEventListener('click', () => send({ cmd: status && status.playing ? 'pause' : 'play' }));
  el.back.addEventListener('click', () => send({ cmd: 'seek_relative', delta: -10 }));
  el.fwd.addEventListener('click', () => send({ cmd: 'seek_relative', delta: 10 }));
  el.prev.addEventListener('click', () => send({ cmd: 'previous' }));
  el.next.addEventListener('click', () => send({ cmd: 'next' }));
  el.close.addEventListener('click', () => send({ cmd: 'stop' }));
  el.recenter.addEventListener('click', () => send({ cmd: 'recenter' }));
  el.mute.addEventListener('click', () => send({ cmd: 'toggle_mute' }));
  el.audio.addEventListener('click', () => openSheet('tracks'));
  el.subtitles.addEventListener('click', () => openSheet('tracks'));
  el.speed.addEventListener('click', () => openSheet('speed'));
  el.upNext.addEventListener('click', () => openSheet('up-next'));
  el.loopVideo.addEventListener('click', () => flip('loop_video', 'set_loop_video'));
  el.autoplay.addEventListener('click', () => flip('autoplay', 'set_autoplay'));
  el.backdrop.addEventListener('click', closeSheet);
  el.sheetClose.addEventListener('click', closeSheet);
  el.sheet.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeSheet(); });
  el.picture.addEventListener('error', () => { el.picture.hidden = true; });

  el.seek.addEventListener('input', () => {
    seeking = true;
    tick();
  });
  el.seek.addEventListener('change', () => {
    const at = Number(el.seek.value);
    send({ cmd: 'seek', position: at }).finally(() => { seeking = false; });
    if (status) { status.position = at; status.sampled_at_ms = Date.now() - clockOffset; }
  });

  el.volume.addEventListener('input', () => {
    changingVolume = true;
    const v = Number(el.volume.value) / 100;
    showVolume(v);
    // A few a second while the finger moves, the last one on letting go.
    if (Date.now() - volumeSentAt > 150) {
      volumeSentAt = Date.now();
      send({ cmd: 'set_volume', volume: v });
    }
  });
  el.volume.addEventListener('change', () => {
    send({ cmd: 'set_volume', volume: Number(el.volume.value) / 100 })
      .finally(() => { changingVolume = false; });
  });

  // Typing into the headset
  function typeResult(text, bad) {
    clearTimeout(typeTimer);
    el.typeStatus.textContent = text;
    el.typeStatus.classList.toggle('bad', !!bad);
    typeTimer = setTimeout(() => { el.typeStatus.textContent = ''; }, bad ? 6000 : 2500);
  }
  el.typeForm.addEventListener('submit', (e) => {
    e.preventDefault();
    const text = el.typeText.value;
    if (!text) return;
    postJson('/api/text', { text: text }).then(() => {
      typeResult(t('remote-sent', 'Sent to headset'), false);
      el.typeText.value = '';
    }, (err) => typeResult(t('remote-not-sent', 'Not sent: {error}', { error: err.message }), true));
  });

  // Live status
  function refreshStatus() {
    return api('/api/status').then((r) => r.json()).then(applyStatus);
  }

  function connect() {
    if (!window.EventSource) {
      setInterval(() => refreshStatus().then(() => setConn(t('remote-connected', 'Connected'), 'ok'), () => {}), 2000);
      return;
    }
    const es = new EventSource('/api/events');
    es.onopen = () => setConn(t('remote-connected', 'Connected'), 'ok');
    es.onmessage = (e) => {
      try { applyStatus(JSON.parse(e.data)); setConn(t('remote-connected', 'Connected'), 'ok'); } catch (_) { /* ignore */ }
    };
    es.onerror = () => {
      if (unpaired) { es.close(); return; }
      setConn(t('remote-reconnecting', 'Reconnecting…'), 'bad');
      // Distinguishes "headset unreachable" from "pairing revoked".
      refreshStatus().catch(() => { if (unpaired) es.close(); });
    };
  }

  setInterval(tick, 250);
  refreshStatus().then(() => { setConn(t('remote-connected', 'Connected'), 'ok'); connect(); }, () => {
    if (!unpaired) { setConn(t('remote-unreachable', 'Headset unreachable'), 'bad'); connect(); }
  });
})();
