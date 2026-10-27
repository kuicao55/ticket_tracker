/* ticket-tracker panel: countdown rings + uPlot charts + HTMX helpers */
(function () {
  const MONO = '11px "JetBrains Mono", "SF Mono", Menlo, monospace';

  function fmtSecs(s) {
    s = Math.max(0, Math.round(s));
    if (s >= 3600) return Math.floor(s / 3600) + 'h' + Math.floor((s % 3600) / 60) + 'm';
    if (s >= 60) return Math.floor(s / 60) + 'm' + String(s % 60).padStart(2, '0') + 's';
    return s + 's';
  }

  function initRings(root) {
    root.querySelectorAll('[data-ring]').forEach(function (el) {
      const last = parseFloat(el.dataset.lastPoll || '0');
      const next = parseFloat(el.dataset.nextPoll || '0');
      const interval = Math.max(1, parseFloat(el.dataset.interval || '90'));
      const circle = el.querySelector('.ring-progress');
      const label = el.querySelector('.ring-value');
      if (!circle || !label) return;
      const r = circle.r.baseVal.value;
      const c = 2 * Math.PI * r;
      circle.style.strokeDasharray = c + ' ' + c;

      function tick() {
        const now = Date.now() / 1000;
        let nextAt = next > 0 ? next : (last > 0 ? last + interval : 0);
        // HTMX 片段刷新有间隔；如果 DOM 里的 next 已经过去，按 interval 向前补齐，
        // 避免在两次刷新之间一直显示 0s。超过两个周期则视为数据过期。
        if (nextAt > 0 && nextAt <= now) {
          const behind = now - nextAt;
          if (behind < interval * 2) {
            nextAt += Math.ceil(behind / interval) * interval;
          } else {
            nextAt = 0;
          }
        }
        let remaining = nextAt > 0 ? nextAt - now : interval;
        if (remaining < 0) remaining = 0;
        const progress = Math.min(1, Math.max(0, 1 - remaining / interval));
        circle.style.strokeDashoffset = String(c * (1 - progress));
        label.textContent = nextAt > 0 ? fmtSecs(remaining) : '…';
      }
      tick();
    });
  }

  function parseTs(t) {
    // SQLite datetime('now') is UTC: "YYYY-MM-DD HH:MM:SS"
    const d = new Date(t.replace(' ', 'T') + (t.indexOf('Z') === -1 ? 'Z' : ''));
    return d.getTime() / 1000;
  }

  function mondayOfWeek(d) {
    const x = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    const diff = (x.getDay() + 6) % 7; // Monday=0 ... Sunday=6
    x.setDate(x.getDate() - diff);
    return x;
  }

  function chartWindow(range) {
    const now = new Date();
    const y = now.getFullYear();
    const m = now.getMonth();
    const d = now.getDate();
    if (range === '7d') {
      const monday = mondayOfWeek(now);
      const nextMonday = new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + 7);
      return [monday.getTime() / 1000, nextMonday.getTime() / 1000];
    }
    if (range === '30d') {
      return [new Date(y, m, d - 29).getTime() / 1000, new Date(y, m, d + 1).getTime() / 1000];
    }
    return [new Date(y, m, d).getTime() / 1000, new Date(y, m, d + 1).getTime() / 1000];
  }

  async function loadChart(el) {
    const url = el.dataset.chartUrl;
    if (!url) return;
    const range = el.dataset.range || '24h';
    const windowRange = chartWindow(range);
    const start = windowRange[0];
    const end = windowRange[1];
    const nowDate = new Date();
    const ny = nowDate.getFullYear();
    const nm = nowDate.getMonth();
    const nd = nowDate.getDate();
    let xTicks = [];
    if (range === '24h') {
      for (let h = 0; h <= 24; h += 4) {
        xTicks.push(new Date(ny, nm, nd, h).getTime() / 1000);
      }
    } else if (range === '7d') {
      const monday = mondayOfWeek(nowDate);
      for (let i = 0; i < 7; i++) {
        xTicks.push(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + i).getTime() / 1000);
      }
    } else {
      for (let i = 0; i <= 30; i += 7) {
        xTicks.push(new Date(ny, nm, nd - 29 + i).getTime() / 1000);
      }
    }
    let points = [];
    try {
      const res = await fetch(url + '?range=' + encodeURIComponent(range));
      const data = await res.json();
      points = (data.points || []).filter(function (p) {
        const x = parseTs(p.t);
        return x >= start && x <= end;
      });
    } catch (e) {
      points = [];
    }
    const empty = el.parentElement.querySelector('.chart-empty');
    el.querySelectorAll('.chart-tooltip').forEach(function (n) { n.remove(); });
    if (!points.length) {
      el.style.display = 'none';
      if (empty) empty.style.display = 'block';
      return;
    }
    el.style.display = 'block';
    if (empty) empty.style.display = 'none';
    const xs = points.map(p => parseTs(p.t));
    const total = points.map(p => p.total);
    const imax = points.map(p => p.imax);
    if (el._uplot) { el._uplot.destroy(); el._uplot = null; }
    const width = Math.max(260, el.clientWidth || 600);
    let tip = null;
    const opts = {
      width: width,
      height: 280,
      padding: [12, 12, 0, 0],
      cursor: { stroke: '#c6613f', drag: { x: false, y: false } },
      scales: {
        x: { time: true, range: () => [start, end] },
        y: { range: (u, min, max) => [0, max == null ? 1 : Math.max(1, max * 1.15)] }
      },
      axes: [
        {
          stroke: '#87867f', font: MONO, size: 38,
          grid: { stroke: '#e3dacc', width: 1 },
          ticks: { stroke: '#cccbc8', width: 1 },
          splits: () => xTicks,
          values: (u, vals) => vals.map(v => {
            const d = new Date(v * 1000);
            if (range === '24h') {
              return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
            }
            if (range === '7d') {
              return ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][d.getDay()];
            }
            return (d.getMonth() + 1) + '/' + d.getDate();
          })
        },
        {
          stroke: '#87867f', font: MONO, size: 34,
          grid: { stroke: '#e3dacc', width: 1 },
          ticks: { stroke: '#cccbc8', width: 1 }
        }
      ],
      series: [
        {},
        { label: 'Total', stroke: '#d97757', width: 2, points: { show: false } },
        { label: 'IMAX', stroke: '#816729', width: 1.5, dash: [5, 4], points: { show: false } }
      ],
      legend: { show: false },
      hooks: {
        setCursor: [function (u) {
          const idx = u.cursor.idx;
          if (!tip || idx == null || !points[idx] || u.cursor.left == null || u.cursor.left < 0) {
            if (tip) tip.style.display = 'none';
            return;
          }
          const p = points[idx];
          const d = new Date(parseTs(p.t) * 1000);
          const time = (d.getMonth() + 1) + '/' + d.getDate() + ' '
            + pad2(d.getHours()) + ':' + pad2(d.getMinutes());
          tip.innerHTML = '<strong>' + time + '</strong><br>基线 ' + p.total
            + ' 场 · IMAX ' + p.imax
            + (p.added > 0 ? '<br>新增 ' + p.added + ' 场' : '');
          tip.style.display = 'block';
          tip.style.left = (u.cursor.left + 12) + 'px';
          tip.style.top = (u.cursor.top + 12) + 'px';
        }]
      },
    };
    el._uplot = new uPlot(opts, [xs, total, imax], el);
    tip = document.createElement('div');
    tip.className = 'chart-tooltip';
    tip.style.display = 'none';
    el.appendChild(tip);
    if (el._ro) el._ro.disconnect();
    el._ro = new ResizeObserver(() => {
      if (el._uplot) el._uplot.setSize({ width: Math.max(260, el.clientWidth), height: 280 });
    });
    el._ro.observe(el);
  }

  function initCharts(root) {
    root.querySelectorAll('.chart[data-chart-url]').forEach(function (el) {
      if (el.dataset.loaded === el.dataset.range) return;
      el.dataset.loaded = el.dataset.range || '24h';
      loadChart(el);
    });
    root.querySelectorAll('[data-chart-range]').forEach(function (btn) {
      if (btn._bound) return;
      btn._bound = true;
      btn.addEventListener('click', function () {
        const card = btn.closest('.chart-card');
        const chart = card && card.querySelector('.chart[data-chart-url]');
        if (!chart) return;
        chart.dataset.range = btn.dataset.chartRange;
        chart.dataset.loaded = '';
        card.querySelectorAll('[data-chart-range]').forEach(b => b.classList.toggle('active', b === btn));
        loadChart(chart);
      });
    });
  }


  function pad2(n) { return String(n).padStart(2, '0'); }
  function localDateInput(d) {
    return d.getFullYear() + '-' + pad2(d.getMonth() + 1) + '-' + pad2(d.getDate());
  }
  function parseDateInput(s) {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(s || ''));
    if (!m) return null;
    return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  }
  function monthStart(d) { return new Date(d.getFullYear(), d.getMonth(), 1); }
  function addMonths(d, n) { return new Date(d.getFullYear(), d.getMonth() + n, 1); }
  function sameDay(a, b) {
    return !!a && !!b && a.getFullYear() === b.getFullYear()
      && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  }
  function debounce(fn, ms) {
    let t = null;
    return function () {
      const args = arguments;
      clearTimeout(t);
      t = setTimeout(function () { fn.apply(null, args); }, ms);
    };
  }
  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  // ---------------- modal pickers (movie / cinema) ----------------
  const CITIES = [
    ['上海', 10], ['北京', 1], ['广州', 20], ['深圳', 30], ['杭州', 50],
    ['成都', 59], ['南京', 55], ['武汉', 57], ['西安', 42], ['重庆', 45],
    ['苏州', 80], ['天津', 40], ['长沙', 70], ['郑州', 73], ['青岛', 60],
    ['厦门', 62], ['福州', 44], ['济南', 96], ['昆明', 114], ['合肥', 56],
    ['宁波', 51], ['东莞', 91], ['佛山', 92], ['无锡', 52]
  ];

  function bindPickerDialogs() {
    bindMovieDialog();
    bindCinemaDialog();
  }

  function bindMovieDialog() {
    const dialog = document.getElementById('movie-dialog');
    if (!dialog || dialog.dataset.bound) return;
    dialog.dataset.bound = '1';
    const trigger = document.querySelector('[data-movie-open]');
    const search = dialog.querySelector('[data-movie-search]');
    const results = dialog.querySelector('[data-movie-results]');
    const value = document.querySelector('[data-movie-value]');
    const label = document.querySelector('[data-movie-label]');
    const idEl = document.querySelector('[data-movie-id]');
    if (!trigger || !search || !results || !value || !label) return;
    let seq = 0;
    let loaded = false;

    function typeLabel(t) {
      return t === 1 ? '热映' : t === 2 ? '即将' : t === 3 ? '经典' : t === 4 ? '搜索' : '';
    }
    function selectMovie(m) {
      value.value = String(m.id);
      label.textContent = m.name;
      if (idEl) idEl.textContent = String(m.id);
      trigger.classList.remove('picker-error');
      dialog.close();
    }
    function render(data) {
      results.textContent = '';
      if (data.remote_error) {
        results.appendChild(el('div', 'menu-note', '猫眼搜索暂时不可用，显示本地缓存结果。'));
      }
      const list = data.movies || [];
      if (!list.length) {
        results.appendChild(el('div', 'menu-empty', '没有找到电影。可以输入更完整的片名，或直接输入猫眼电影 ID。'));
        return;
      }
      list.forEach(function (m) {
        const row = el('button', 'menu-row');
        row.type = 'button';
        row.dataset.movieOption = String(m.id);
        const main = el('span', 'menu-row-main');
        main.appendChild(el('span', 'menu-row-name', m.name));
        main.appendChild(el('span', 'menu-row-sub mono',
          String(m.id) + (typeLabel(m.show_type) ? ' · ' + typeLabel(m.show_type) : '')));
        row.appendChild(main);
        if (String(value.value) === String(m.id)) row.classList.add('selected');
        row.addEventListener('click', function () { selectMovie(m); });
        results.appendChild(row);
      });
    }
    async function load(q) {
      const my = ++seq;
      results.textContent = '';
      results.appendChild(el('div', 'menu-empty', '搜索中…'));
      try {
        const res = await fetch('/api/v1/movies/search?q=' + encodeURIComponent(q || ''));
        if (!res.ok) throw new Error('HTTP ' + res.status);
        const data = await res.json();
        if (my !== seq) return;
        render(data);
        loaded = true;
      } catch (e) {
        if (my !== seq) return;
        results.textContent = '';
        results.appendChild(el('div', 'menu-empty', '搜索失败，请稍后重试。'));
      }
    }
    trigger.addEventListener('click', function () {
      trigger.classList.remove('picker-error');
      if (!dialog.open) dialog.showModal();
      search.focus();
      if (!loaded) load('');
    });
    search.addEventListener('input', debounce(function () { load(search.value.trim()); }, 350));
    const close = dialog.querySelector('[data-movie-close]');
    if (close) close.addEventListener('click', function () { dialog.close(); });
    dialog.addEventListener('click', function (e) { if (e.target === dialog) dialog.close(); });
  }

  function bindCinemaDialog() {
    const dialog = document.getElementById('cinema-dialog');
    if (!dialog || dialog.dataset.bound) return;
    dialog.dataset.bound = '1';
    const trigger = document.querySelector('[data-cinema-open]');
    const close = dialog.querySelector('[data-cinema-close]');
    const done = dialog.querySelector('[data-cinema-done]');
    const search = dialog.querySelector('[data-cinema-search]');
    const results = dialog.querySelector('[data-cinema-results]');
    const hidden = document.querySelector('[data-cinema-value]');
    const chips = document.querySelector('[data-cinema-chips]');
    const label = document.querySelector('[data-cinema-label]');
    const hint = dialog.querySelector('[data-cinema-hint]');
    const citySelect = dialog.querySelector('[data-cinema-city]');
    if (!trigger || !search || !results || !hidden || !label) return;
    const selected = new Map();
    let seq = 0;

    if (citySelect) {
      CITIES.forEach(function (c) {
        const opt = document.createElement('option');
        opt.value = String(c[1]);
        opt.textContent = c[0];
        citySelect.appendChild(opt);
      });
      const def = parseInt(dialog.dataset.cityId || '10', 10) || 10;
      citySelect.value = String(def);
    }

    function rows() { return Array.from(results.querySelectorAll('[data-cinema-option]')); }
    function rowFor(id) {
      return rows().find(function (r) { return String(r.dataset.id) === String(id); });
    }
    function sync() {
      hidden.value = Array.from(selected.keys()).join(',');
      if (selected.size === 0) {
        label.textContent = '选择影院';
      } else if (selected.size === 1) {
        label.textContent = Array.from(selected.values())[0];
      } else {
        label.textContent = '已选 ' + selected.size + ' 家';
      }
      if (chips) {
        chips.textContent = '';
        selected.forEach(function (name, id) {
          const chip = el('button', 'chip', name + ' ×');
          chip.type = 'button';
          chip.addEventListener('click', function (e) {
            e.stopPropagation();
            selected.delete(id);
            const row = rowFor(id);
            if (row) {
              const cb = row.querySelector('input[type=checkbox]');
              if (cb) cb.checked = false;
            }
            sync();
          });
          chips.appendChild(chip);
        });
      }
      if (hidden.value) trigger.classList.remove('picker-error');
      if (hint) hint.textContent = selected.size ? '已选 ' + selected.size + ' 家影院' : '可多选；支持猫眼影院搜索';
    }
    function bindRow(row) {
      const cb = row.querySelector('input[type=checkbox]');
      if (!cb) return;
      cb.addEventListener('change', function () {
        const id = String(row.dataset.id);
        const name = row.dataset.name || id;
        if (cb.checked) selected.set(id, name); else selected.delete(id);
        sync();
      });
    }
    rows().forEach(function (row) {
      const cb = row.querySelector('input[type=checkbox]');
      if (cb && cb.checked) selected.set(String(row.dataset.id), row.dataset.name || row.dataset.id);
      bindRow(row);
    });
    function renderRemote(list) {
      results.querySelectorAll('[data-remote-cinema]').forEach(function (n) { n.remove(); });
      const existing = new Set(rows().filter(function (r) { return !r.dataset.remoteCinema; })
        .map(function (r) { return String(r.dataset.id); }));
      (list || []).forEach(function (c) {
        if (existing.has(String(c.id))) return;
        existing.add(String(c.id));
        const row = el('label', 'menu-row');
        row.dataset.cinemaOption = '';
        row.dataset.remoteCinema = '1';
        row.dataset.id = String(c.id);
        row.dataset.name = c.name;
        row.dataset.search = [c.name, c.id, c.addr].filter(Boolean).join(' ');
        const cb = document.createElement('input');
        cb.type = 'checkbox';
        cb.value = String(c.id);
        cb.checked = selected.has(String(c.id));
        row.appendChild(cb);
        const main = el('span', 'menu-row-main');
        main.appendChild(el('span', 'menu-row-name', c.name));
        main.appendChild(el('span', 'menu-row-sub mono',
          String(c.id) + (c.addr ? ' · ' + c.addr : '')));
        row.appendChild(main);
        bindRow(row);
        results.appendChild(row);
      });
    }
    function filterLocal(q) {
      rows().forEach(function (row) {
        if (row.dataset.remoteCinema) return;
        row.hidden = q !== '' && !String(row.dataset.search || '').toLowerCase().includes(q);
      });
    }
    async function remoteSearch(q) {
      const my = ++seq;
      if (!q) { renderRemote([]); return; }
      const city = citySelect ? citySelect.value : '';
      try {
        const res = await fetch('/api/v1/cinemas/search?q=' + encodeURIComponent(q)
          + (city ? '&city=' + encodeURIComponent(city) : ''));
        if (!res.ok) throw new Error('HTTP ' + res.status);
        const data = await res.json();
        if (my !== seq) return;
        renderRemote(data.cinemas || []);
      } catch (e) {
        if (my !== seq) return;
        renderRemote([]);
      }
    }
    const debouncedRemote = debounce(remoteSearch, 350);
    search.addEventListener('input', function () {
      const q = search.value.trim();
      filterLocal(q.toLowerCase());
      debouncedRemote(q);
    });
    if (citySelect) {
      citySelect.addEventListener('change', function () {
        // 记住城市，下次打开还用这个城市。
        fetch('/api/v1/settings', {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ maoyan_city_id: Number(citySelect.value) })
        }).catch(function () {});
        remoteSearch(search.value.trim());
      });
    }
    trigger.addEventListener('click', function () {
      trigger.classList.remove('picker-error');
      if (!dialog.open) dialog.showModal();
      sync();
      search.focus();
    });
    if (close) close.addEventListener('click', function () { dialog.close(); });
    if (done) done.addEventListener('click', function () { dialog.close(); });
    dialog.addEventListener('click', function (e) { if (e.target === dialog) dialog.close(); });
    sync();
  }

  // ---------------- hotel-style date range dialog ----------------
  const dateState = { start: null, end: null, hover: null, view: null, apply: null };

  function initDateDialog() {
    if (window._dateDialogInit) return;
    window._dateDialogInit = true;
    const dialog = document.getElementById('date-dialog');
    if (!dialog) return;
    const close = dialog.querySelector('[data-date-close]');
    const prev = dialog.querySelector('[data-date-prev]');
    const next = dialog.querySelector('[data-date-next]');
    const clear = dialog.querySelector('[data-date-clear]');
    const apply = dialog.querySelector('[data-date-apply]');
    if (close) close.addEventListener('click', function () { dialog.close(); });
    if (prev) prev.addEventListener('click', function () {
      dateState.view = addMonths(dateState.view || monthStart(new Date()), -1);
      renderDateDialog();
    });
    if (next) next.addEventListener('click', function () {
      dateState.view = addMonths(dateState.view || monthStart(new Date()), 1);
      renderDateDialog();
    });
    if (clear) clear.addEventListener('click', function () {
      dateState.start = null;
      dateState.end = null;
      dateState.hover = null;
      renderDateDialog();
    });
    if (apply) apply.addEventListener('click', function () {
      if (!dateState.start) return;
      const end = dateState.end || dateState.start;
      const cb = dateState.apply;
      const s = localDateInput(dateState.start);
      const e = localDateInput(end);
      dialog.close();
      if (cb) cb(s, e);
    });
    dialog.addEventListener('click', function (e) {
      if (e.target === dialog) dialog.close();
    });
  }

  function openDateDialog(startStr, endStr, apply) {
    const dialog = document.getElementById('date-dialog');
    if (!dialog) {
      if (apply) apply(startStr || localDateInput(new Date()), endStr || startStr || localDateInput(new Date()));
      return;
    }
    const start = parseDateInput(startStr);
    const end = parseDateInput(endStr);
    dateState.start = start;
    dateState.end = end;
    dateState.hover = null;
    dateState.apply = apply;
    dateState.view = monthStart(start || new Date());
    renderDateDialog();
    if (!dialog.open) dialog.showModal();
  }

  function renderDateDialog() {
    const dialog = document.getElementById('date-dialog');
    if (!dialog) return;
    const months = dialog.querySelector('[data-date-months]');
    const title = dialog.querySelector('[data-date-title]');
    const hint = dialog.querySelector('[data-date-hint]');
    if (!months) return;
    const view = dateState.view || monthStart(new Date());
    const next = addMonths(view, 1);
    if (title) {
      title.textContent = view.getFullYear() + '年' + (view.getMonth() + 1) + '月 — '
        + next.getFullYear() + '年' + (next.getMonth() + 1) + '月';
    }
    let previewStart = dateState.start;
    let previewEnd = dateState.end;
    if (previewStart && !previewEnd && dateState.hover) {
      if (dateState.hover < previewStart) {
        previewEnd = previewStart;
        previewStart = dateState.hover;
      } else {
        previewEnd = dateState.hover;
      }
    }
    months.textContent = '';
    months.appendChild(renderMonth(view.getFullYear(), view.getMonth(), previewStart, previewEnd));
    months.appendChild(renderMonth(next.getFullYear(), next.getMonth(), previewStart, previewEnd));
    if (hint) {
      if (dateState.start && !dateState.end) hint.textContent = '再选择结束日期';
      else if (dateState.start && dateState.end) {
        const days = Math.round((dateState.end - dateState.start) / 86400000) + 1;
        hint.textContent = '已选 ' + days + ' 天';
      } else hint.textContent = '先选开始日期，再选结束日期';
    }
  }

  function updateDateRangeClasses() {
    const start = dateState.start;
    const end = dateState.end;
    let a = start;
    let b = end;
    if (start && !end && dateState.hover) {
      if (dateState.hover < start) { a = dateState.hover; b = start; }
      else { a = start; b = dateState.hover; }
    }
    document.querySelectorAll('#date-dialog .cal-day[data-date]').forEach(function (btn) {
      const d = parseDateInput(btn.dataset.date);
      btn.classList.toggle('is-start', sameDay(d, a));
      btn.classList.toggle('is-end', sameDay(d, b));
      btn.classList.toggle('in-range', !!a && !!b && d > a && d < b);
    });
  }

  function renderMonth(year, month, rangeStart, rangeEnd) {
    const wrap = el('div', 'cal-month');
    wrap.appendChild(el('div', 'cal-title', year + '年' + (month + 1) + '月'));
    const week = el('div', 'cal-weekdays');
    ['一', '二', '三', '四', '五', '六', '日'].forEach(function (w) {
      week.appendChild(el('span', '', w));
    });
    wrap.appendChild(week);
    const grid = el('div', 'cal-grid');
    const first = new Date(year, month, 1);
    const offset = (first.getDay() + 6) % 7;
    const days = new Date(year, month + 1, 0).getDate();
    const today = new Date();
    for (let i = 0; i < 42; i++) {
      if (i < offset || i >= offset + days) {
        grid.appendChild(el('span', 'cal-day blank'));
        continue;
      }
      const d = new Date(year, month, i - offset + 1);
      const ds = localDateInput(d);
      const btn = el('button', 'cal-day', String(d.getDate()));
      btn.type = 'button';
      btn.dataset.date = ds;
      if (sameDay(d, today)) btn.classList.add('is-today');
      if (rangeStart && sameDay(d, rangeStart)) btn.classList.add('is-start');
      if (rangeEnd && sameDay(d, rangeEnd)) btn.classList.add('is-end');
      if (rangeStart && rangeEnd && d > rangeStart && d < rangeEnd) btn.classList.add('in-range');
      btn.addEventListener('click', function () {
        const picked = parseDateInput(ds);
        if (!dateState.start || dateState.end) {
          dateState.start = picked;
          dateState.end = null;
        } else if (picked < dateState.start) {
          dateState.start = picked;
          dateState.end = null;
        } else {
          dateState.end = picked;
        }
        dateState.hover = null;
        renderDateDialog();
      });
      btn.addEventListener('mouseenter', function () {
        if (dateState.start && !dateState.end) {
          dateState.hover = d;
          updateDateRangeClasses();
        }
      });
      grid.appendChild(btn);
    }
    grid.addEventListener('mouseleave', function () {
      if (dateState.start && !dateState.end) {
        dateState.hover = null;
        updateDateRangeClasses();
      }
    });
    wrap.appendChild(grid);
    return wrap;
  }

  // ---------------- GitHub-style hour grid + date segments ----------------
  function initTimePickers(root) {
    if (!window._hourDragBound) {
      window._hourDragBound = true;
      document.addEventListener('pointerup', function () {
        const drag = window._hourDrag;
        if (!drag) return;
        if (drag.selected.size === 0) drag.selected.add(drag.anchor);
        drag.render();
        drag.onChange();
        window._hourDrag = null;
      });
    }
    root.querySelectorAll('[data-time-picker]').forEach(function (p) {
      if (p.dataset.timeInit) return;
      p.dataset.timeInit = '1';
      const hidden = p.querySelector('[data-time-value]');
      const modes = Array.from(p.querySelectorAll('[data-time-mode]'));
      const panels = {};
      p.querySelectorAll('[data-time-panel]').forEach(function (n) { panels[n.dataset.timePanel] = n; });
      const dailyWrap = p.querySelector('[data-daily-grid]');
      const segmentsWrap = p.querySelector('[data-time-segments]');
      const addBtn = p.querySelector('[data-time-add]');
      let mode = 'all';
      let dailySelected = new Set([19, 20, 21]);
      let dailyCtrl = null;
      const segments = [];

      function hourGrid(container, selected, onChange) {
        container.textContent = '';
        const buttons = [];
        function render() {
          buttons.forEach(function (b) {
            b.classList.toggle('on', selected.has(Number(b.dataset.hour)));
            b.setAttribute('aria-pressed', selected.has(Number(b.dataset.hour)) ? 'true' : 'false');
          });
        }
        function applyRange(a, b, add) {
          const lo = Math.min(a, b);
          const hi = Math.max(a, b);
          for (let h = lo; h <= hi; h++) {
            if (add) selected.add(h); else selected.delete(h);
          }
          render();
        }
        for (let h = 0; h < 24; h++) {
          const b = el('button', 'hour-cell', pad2(h));
          b.type = 'button';
          b.dataset.hour = String(h);
          b.title = pad2(h) + ':00 – ' + pad2(h + 1) + ':00';
          b.addEventListener('pointerdown', function (e) {
            e.preventDefault();
            const add = !selected.has(h);
            window._hourDrag = {
              selected: selected, anchor: h, add: add,
              render: render, onChange: onChange,
            };
            applyRange(h, h, add);
          });
          b.addEventListener('pointerenter', function () {
            const drag = window._hourDrag;
            if (!drag || drag.selected !== selected) return;
            applyRange(drag.anchor, h, drag.add);
          });
          b.addEventListener('click', function (e) {
            if (e.detail !== 0) return;
            if (selected.has(h)) selected.delete(h); else selected.add(h);
            if (selected.size === 0) selected.add(h);
            render();
            onChange();
          });
          buttons.push(b);
          container.appendChild(b);
        }
        render();
        return { selected: selected, render: render };
      }

      function dateLabel(start, end) {
        return start === end ? start : start + ' → ' + end;
      }

      function createSegment(start, end, hours) {
        const seg = { el: el('div', 'time-segment'), start: start, end: end, selected: new Set(hours) };
        const head = el('div', 'time-segment-head');
        const dateBtn = el('button', 'date-trigger');
        dateBtn.type = 'button';
        dateBtn.dataset.dateTrigger = '';
        const icon = el('span', 'date-trigger-icon');
        icon.innerHTML = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><rect x="2.5" y="3.5" width="11" height="10" rx="2"/><path d="M2.5 6.5h11M5.5 2.5v2M10.5 2.5v2"/></svg>';
        dateBtn.appendChild(icon);
        const label = el('span', '', dateLabel(start, end));
        dateBtn.appendChild(label);
        dateBtn.addEventListener('click', function (e) {
          e.preventDefault();
          openDateDialog(seg.start, seg.end, function (s, e2) {
            seg.start = s;
            seg.end = e2;
            label.textContent = dateLabel(s, e2);
            update();
          });
        });
        head.appendChild(dateBtn);
        const remove = el('button', 'btn ghost small', '移除');
        remove.type = 'button';
        remove.addEventListener('click', function (e) {
          e.preventDefault();
          const idx = segments.indexOf(seg);
          if (idx >= 0) segments.splice(idx, 1);
          seg.el.remove();
          if (segments.length === 0) createSegment(localDateInput(new Date()), localDateInput(new Date()), [19, 20, 21]);
          update();
        });
        head.appendChild(remove);
        seg.el.appendChild(head);
        const grid = el('div', 'hour-grid');
        seg.el.appendChild(grid);
        seg.grid = hourGrid(grid, seg.selected, update);
        segmentsWrap.appendChild(seg.el);
        segments.push(seg);
        return seg;
      }

      function update() {
        let obj;
        if (mode === 'all') {
          obj = { type: 'all' };
        } else if (mode === 'daily') {
          const hours = Array.from(dailySelected).sort(function (a, b) { return a - b; });
          obj = { type: 'daily_hours', hours: hours.length ? hours : [0] };
        } else {
          const intervals = [];
          segments.forEach(function (seg) {
            const hours = Array.from(seg.selected).sort(function (a, b) { return a - b; });
            if (!hours.length) return;
            let runStart = hours[0];
            let prev = hours[0];
            function push() {
              intervals.push({
                start_date: seg.start, start_hour: runStart,
                end_date: seg.end, end_hour: Math.min(24, prev + 1)
              });
            }
            for (let i = 1; i < hours.length; i++) {
              if (hours[i] === prev + 1) {
                prev = hours[i];
              } else {
                push();
                runStart = hours[i];
                prev = hours[i];
              }
            }
            push();
          });
          obj = { type: 'intervals', intervals: intervals };
        }
        hidden.value = JSON.stringify(obj);
      }

      function ensureDaily() {
        if (!dailyCtrl) {
          dailyCtrl = hourGrid(dailyWrap, dailySelected, update);
        }
      }
      function setMode(next) {
        mode = next;
        modes.forEach(function (b) { b.classList.toggle('active', b.dataset.timeMode === next); });
        Object.keys(panels).forEach(function (k) { panels[k].hidden = k !== next; });
        if (next === 'daily') ensureDaily();
        if (next === 'dates' && segments.length === 0) {
          createSegment(localDateInput(new Date()), localDateInput(new Date()), [19, 20, 21]);
        }
        update();
      }

      let initial = null;
      try { initial = JSON.parse(hidden.value || '{}'); } catch (e) { initial = null; }
      if (initial && initial.type === 'daily_hours' && Array.isArray(initial.hours) && initial.hours.length) {
        dailySelected = new Set(initial.hours.map(Number));
        setMode('daily');
      } else if (initial && initial.type === 'intervals' && Array.isArray(initial.intervals) && initial.intervals.length) {
        initial.intervals.forEach(function (it) {
          const hours = [];
          for (let h = it.start_hour; h < it.end_hour; h++) hours.push(h);
          if (!hours.length) hours.push(Number(it.start_hour));
          createSegment(it.start_date, it.end_date, hours);
        });
        setMode('dates');
      } else {
        setMode('all');
      }
      modes.forEach(function (b) {
        b.addEventListener('click', function () { setMode(b.dataset.timeMode); });
      });
      if (addBtn) addBtn.addEventListener('click', function () {
        createSegment(localDateInput(new Date()), localDateInput(new Date()), [19, 20, 21]);
        update();
      });
    });
  }

  function initFormGuards(root) {
    root.querySelectorAll('form[data-picker-form]').forEach(function (form) {
      if (form.dataset.guardInit) return;
      form.dataset.guardInit = '1';
      form.addEventListener('submit', function (e) {
        const movie = form.querySelector('[data-movie-value]');
        const cinemas = form.querySelector('[data-cinema-value]');
        if (movie && !movie.value) {
          e.preventDefault();
          const btn = form.querySelector('[data-movie-open]');
          if (btn) {
            btn.classList.add('picker-error');
            btn.click();
          }
          return;
        }
        if (cinemas && !cinemas.value) {
          e.preventDefault();
          const btn = form.querySelector('[data-cinema-open]');
          if (btn) {
            btn.classList.add('picker-error');
            btn.click();
          }
        }
      });
    });
  }

  function initBaselineTabs(root) {
    root.querySelectorAll('[data-baseline-tabs]').forEach(function (tabs) {
      if (tabs.dataset.baselineInit) return;
      tabs.dataset.baselineInit = '1';
      const card = tabs.closest('.baseline-shows');
      if (!card) return;
      const buttons = Array.from(tabs.querySelectorAll('[data-baseline-tab]'));
      const days = Array.from(card.querySelectorAll('[data-date-index]'));
      if (!buttons.length) return;
      function select(index) {
        buttons.forEach(function (btn, i) {
          const on = i === index;
          btn.classList.toggle('active', on);
          btn.setAttribute('aria-selected', on ? 'true' : 'false');
        });
        days.forEach(function (day, i) {
          day.hidden = i !== index;
        });
      }
      buttons.forEach(function (btn, i) {
        btn.addEventListener('click', function () { select(i); });
      });
      select(0);
    });
  }

  function init(root) {
    initRings(root);
    initCharts(root);
    bindPickerDialogs();
    initDateDialog();
    initTimePickers(root);
    initFormGuards(root);
    initBaselineTabs(root);
  }

  setInterval(() => initRings(document), 250);
  setInterval(() => {
    document.querySelectorAll('.chart[data-chart-url]').forEach(loadChart);
  }, 60000);
  document.addEventListener('DOMContentLoaded', () => init(document));
  // outerHTML swap 后旧的 target 可能已脱离 DOM；必须重新扫描整个 document，
  // 否则新插入的倒计时圈会短暂显示模板里的占位符。
  document.body.addEventListener('htmx:afterSwap', () => init(document));
  document.body.addEventListener('htmx:afterSettle', () => init(document));
  document.body.addEventListener('htmx:afterRequest', (e) => {
    const el = e.detail && e.detail.elt;
    const dlg = el && el.closest && el.closest('dialog');
    if (dlg) dlg.close();
  });
})();
