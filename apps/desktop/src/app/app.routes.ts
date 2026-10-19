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
    path: 'events',
    title: 'Event log · JARVIS',
    loadComponent: () => import('./features/events/events').then((m) => m.Events),
  },
  {
    path: 'settings',
    title: 'Settings · JARVIS',
    loadComponent: () => import('./features/settings/settings').then((m) => m.SettingsPage),
  },
  { path: '**', redirectTo: 'dashboard' },
];
