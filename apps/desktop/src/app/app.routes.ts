import type { Routes } from '@angular/router';

export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'dashboard' },
  {
    path: 'dashboard',
    title: 'Dashboard · JARVIS',
    loadComponent: () => import('./features/dashboard/dashboard').then((m) => m.Dashboard),
  },
  {
    path: 'chat',
    title: 'Chat · JARVIS',
    loadComponent: () => import('./features/chat/chat').then((m) => m.Chat),
  },
  {
    path: 'models',
    title: 'Models · JARVIS',
    loadComponent: () => import('./features/models/models').then((m) => m.Models),
  },
  {
    path: 'runtime',
    title: 'Local runtime · JARVIS',
    loadComponent: () => import('./features/runtime/runtime').then((m) => m.RuntimePage),
  },
  {
    path: 'agents',
    title: 'Agents · JARVIS',
    loadComponent: () => import('./features/agents/agents').then((m) => m.AgentsPage),
  },
  {
    path: 'memory',
    title: 'Memory · JARVIS',
    loadComponent: () => import('./features/memory/memory').then((m) => m.MemoryPage),
  },
  {
    path: 'plugins',
    title: 'Plugins · JARVIS',
    loadComponent: () => import('./features/plugins/plugins').then((m) => m.PluginsPage),
  },
  {
    path: 'events',
    title: 'Event log · JARVIS',
    loadComponent: () => import('./features/events/events').then((m) => m.Events),
  },
  {
    path: 'settings/voice',
    title: 'Voice · JARVIS',
    loadComponent: () => import('./features/voice/voice').then((m) => m.VoicePage),
  },
  {
    path: 'settings',
    title: 'Settings · JARVIS',
    loadComponent: () => import('./features/settings/settings').then((m) => m.SettingsPage),
  },
  { path: '**', redirectTo: 'dashboard' },
];
